//! Screen recording: desktop duplication frames -> hardware H.264, system
//! audio + mic -> AAC, muxed to MP4 by the Media Foundation sink writer.
//!
//! Everything runs on one worker thread with its own D3D device, so no COM
//! object crosses threads. Frames stay on the GPU: each one is copied out of
//! the duplicated desktop into a small texture pool, the cursor is drawn on
//! top with GDI, and the texture goes straight to the encoder.
//!
//! The recording pill and frame use WDA_EXCLUDEFROMCAPTURE, so desktop
//! duplication never sees them.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{Interface, Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::Rect;

pub const WM_REC_DONE: u32 = WM_APP + 5;

const RATE: u32 = 48_000;
const HNS: i64 = 10_000_000;

pub struct Settings {
    pub area: Rect,
    pub fps: u32,
    /// Megabits per second.
    pub mbps: u32,
    pub mic: bool,
    pub system_audio: bool,
    pub cursor: bool,
    pub out: PathBuf,
}

/// Shared between the UI thread and the recorder thread.
pub struct Control {
    pub stop: AtomicBool,
    /// Set by the UI when the countdown ends. Setup happens before this.
    pub go: AtomicBool,
    /// Set by the recorder when setup is done and it's waiting for `go`.
    pub ready: AtomicBool,
    /// Set once the first frame is written.
    pub live: AtomicBool,
    pub paused: AtomicBool,
    pub mic_muted: AtomicBool,
    clock: Mutex<Clock>,
}

struct Clock {
    start: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

impl Control {
    fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            go: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            live: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            mic_muted: AtomicBool::new(false),
            clock: Mutex::new(Clock { start: Instant::now(), paused_at: None, paused_total: Duration::ZERO }),
        }
    }

    fn restart_clock(&self) {
        *self.clock.lock().unwrap() = Clock { start: Instant::now(), paused_at: None, paused_total: Duration::ZERO };
    }

    /// Recording time so far, not counting pauses.
    pub fn elapsed(&self) -> Duration {
        let c = self.clock.lock().unwrap();
        let now = c.paused_at.unwrap_or_else(Instant::now);
        now.saturating_duration_since(c.start).saturating_sub(c.paused_total)
    }

    pub fn set_paused(&self, p: bool) {
        let mut c = self.clock.lock().unwrap();
        match (p, c.paused_at) {
            (true, None) => c.paused_at = Some(Instant::now()),
            (false, Some(t)) => {
                c.paused_total += t.elapsed();
                c.paused_at = None;
            }
            _ => {}
        }
        self.paused.store(p, Ordering::SeqCst);
    }
}

pub struct Done {
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub seconds: f64,
}

pub fn start(s: Settings, notify: HWND) -> Arc<Control> {
    let target = notify.0 as isize;
    start_with(s, move |done| unsafe {
        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_REC_DONE, WPARAM(0), LPARAM(Box::into_raw(Box::new(done)) as isize));
    })
}

/// Start recording; `on_done` runs on the recorder thread when it ends.
pub fn start_with(s: Settings, on_done: impl FnOnce(Done) + Send + 'static) -> Arc<Control> {
    let ctl = Arc::new(Control::new());
    let c2 = ctl.clone();
    std::thread::Builder::new()
        .name("fastsnip-rec".into())
        .spawn(move || {
            let out = s.out.clone();
            let res = unsafe { run(&s, &c2) };
            let secs = c2.elapsed().as_secs_f64();
            let done = match res {
                Ok(()) => Done { path: Some(out), error: None, seconds: secs },
                Err(e) => {
                    let _ = std::fs::remove_file(&out);
                    Done { path: None, error: Some(e.message().to_string()), seconds: secs }
                }
            };
            on_done(done);
        })
        .expect("rec thread");
    ctl
}

// ------------------------------------------------------------------ audio

struct Source {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    queue: VecDeque<f32>,
}

fn float_format() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: 2,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: 3,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

unsafe fn open_source(loopback: bool) -> Result<Source> {
    let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let dev = en.GetDefaultAudioEndpoint(if loopback { eRender } else { eCapture }, eConsole)?;
    let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
    let fmt = float_format();
    let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt as *const _ as *const WAVEFORMATEX, None)?;
    let capture: IAudioCaptureClient = client.GetService()?;
    client.Start()?;
    Ok(Source { client, capture, queue: VecDeque::new() })
}

unsafe fn drain(src: &mut Source) {
    loop {
        let Ok(n) = src.capture.GetNextPacketSize() else { return };
        if n == 0 {
            return;
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        if src.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
            return;
        }
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        let samples = frames as usize * 2;
        if silent || data.is_null() {
            src.queue.extend(std::iter::repeat_n(0.0, samples));
        } else {
            src.queue.extend(std::slice::from_raw_parts(data as *const f32, samples));
        }
        let _ = src.capture.ReleaseBuffer(frames);
        // Never let a source run more than half a second ahead.
        let max = RATE as usize;
        if src.queue.len() > max {
            let drop = src.queue.len() - max;
            src.queue.drain(..drop);
        }
    }
}

// ------------------------------------------------------------------ media types

unsafe fn video_type(sub: &windows::core::GUID, w: u32, h: u32, fps: u32, bitrate: u32) -> Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    t.SetGUID(&MF_MT_SUBTYPE, sub)?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
    t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
    t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    if bitrate > 0 {
        t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
    } else {
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
    }
    Ok(t)
}

unsafe fn audio_type(aac: bool) -> Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    t.SetGUID(&MF_MT_SUBTYPE, if aac { &MFAudioFormat_AAC } else { &MFAudioFormat_PCM })?;
    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    if aac {
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?; // 192 kbps
    } else {
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE * 4)?;
    }
    Ok(t)
}

struct Writer {
    sink: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
    gpu: bool,
}

unsafe fn make_writer(path: &HSTRING, s: &Settings, w: u32, h: u32, manager: Option<&IMFDXGIDeviceManager>, audio: bool) -> Result<Writer> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 4)?;
    let attrs = attrs.unwrap();
    attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
    if let Some(m) = manager {
        attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, m)?;
    }
    let sink = MFCreateSinkWriterFromURL(path, None, &attrs)?;
    let bitrate = s.mbps.clamp(1, 200) * 1_000_000;
    let video = sink.AddStream(&video_type(&MFVideoFormat_H264, w, h, s.fps, bitrate)?)?;
    sink.SetInputMediaType(video, &video_type(&MFVideoFormat_ARGB32, w, h, s.fps, 0)?, None)?;
    let audio = if audio {
        let a = sink.AddStream(&audio_type(true)?)?;
        sink.SetInputMediaType(a, &audio_type(false)?, None)?;
        Some(a)
    } else {
        None
    };
    sink.BeginWriting()?;
    Ok(Writer { sink, video, audio, gpu: manager.is_some() })
}

// ------------------------------------------------------------------ main loop

unsafe fn texture(dev: &ID3D11Device, w: u32, h: u32, staging: bool) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        BindFlags: if staging { 0 } else { (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32 },
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        MiscFlags: if staging { 0 } else { D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32 },
    };
    let mut t = None;
    dev.CreateTexture2D(&desc, None, Some(&mut t))?;
    Ok(t.unwrap())
}

/// One display the recording area touches.
struct Screen {
    dup: IDXGIOutputDuplication,
    rect: Rect,
    desk: Option<ID3D11Texture2D>,
}

/// The GPU that drives most of the area, and its displays that overlap it.
/// Laptops with two GPUs can have displays on each; we record from one.
unsafe fn pick_adapter(area: &Rect) -> Result<(IDXGIAdapter1, Vec<(IDXGIOutput1, Rect)>)> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
    let mut best: Option<(IDXGIAdapter1, Vec<(IDXGIOutput1, Rect)>)> = None;
    let mut best_px = 0i64;
    let mut a = 0;
    while let Ok(adapter) = factory.EnumAdapters1(a) {
        a += 1;
        let mut outs = Vec::new();
        let mut px = 0i64;
        let mut o = 0;
        while let Ok(out) = adapter.EnumOutputs(o) {
            o += 1;
            let Ok(d) = out.GetDesc() else { continue };
            if !d.AttachedToDesktop.as_bool() {
                continue;
            }
            let r = Rect::from_win(d.DesktopCoordinates);
            if let Some(i) = r.intersect(area) {
                px += i.w as i64 * i.h as i64;
                if let Ok(o1) = out.cast::<IDXGIOutput1>() {
                    outs.push((o1, r));
                }
            }
        }
        if px > best_px {
            best_px = px;
            best = Some((adapter, outs));
        }
    }
    best.ok_or_else(|| windows::core::Error::new(DXGI_ERROR_NOT_FOUND, "The recording area isn't on any display"))
}

/// Pull the newest image from each display. Waits up to `wait_ms` on the first only.
unsafe fn poll(screens: &mut [Screen], dev: &ID3D11Device, ctx: &ID3D11DeviceContext, wait_ms: u32) -> Result<u32> {
    let mut got = 0;
    for (i, sc) in screens.iter_mut().enumerate() {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        match sc.dup.AcquireNextFrame(if i == 0 { wait_ms } else { 0 }, &mut info, &mut res) {
            Ok(()) => {
                if let Some(t) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                    if sc.desk.is_none() {
                        let mut d = D3D11_TEXTURE2D_DESC::default();
                        t.GetDesc(&mut d);
                        sc.desk = Some(texture(dev, d.Width, d.Height, false)?);
                    }
                    ctx.CopyResource(sc.desk.as_ref().unwrap(), &t);
                    got += 1;
                }
                let _ = sc.dup.ReleaseFrame();
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(e) => return at(Err(e), "next desktop frame"),
        }
    }
    Ok(got)
}

struct CursorCache {
    handle: isize,
    hot: (i32, i32),
}

unsafe fn draw_cursor(tex: &ID3D11Texture2D, area: &Rect, cache: &mut CursorCache) {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
        return;
    }
    if ci.hCursor.0 as isize != cache.handle {
        let mut ii = ICONINFO::default();
        if GetIconInfo(ci.hCursor.into(), &mut ii).is_ok() {
            cache.hot = (ii.xHotspot as i32, ii.yHotspot as i32);
            if !ii.hbmMask.is_invalid() {
                let _ = DeleteObject(ii.hbmMask.into());
            }
            if !ii.hbmColor.is_invalid() {
                let _ = DeleteObject(ii.hbmColor.into());
            }
        }
        cache.handle = ci.hCursor.0 as isize;
    }
    let pos: POINT = ci.ptScreenPos;
    let (x, y) = (pos.x - cache.hot.0 - area.x, pos.y - cache.hot.1 - area.y);
    if x < -64 || y < -64 || x > area.w || y > area.h {
        return;
    }
    let Ok(surf) = tex.cast::<IDXGISurface1>() else { return };
    if let Ok(dc) = surf.GetDC(false) {
        let _ = DrawIconEx(dc, x, y, ci.hCursor.into(), 0, 0, 0, None, DI_NORMAL);
        let _ = surf.ReleaseDC(None);
    }
}

unsafe fn run(s: &Settings, ctl: &Control) -> Result<()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    at(MFStartup(MF_VERSION, MFSTARTUP_FULL), "media foundation")?;
    let result = record(s, ctl);
    let _ = MFShutdown();
    result
}

unsafe fn record(s: &Settings, ctl: &Control) -> Result<()> {
    let t_setup = Instant::now();
    let log = crate::overlay::log;
    // Own device, with video support for the hardware encoder.
    let (adapter, outputs) = at(pick_adapter(&s.area), "find display")?;
    let mut dev: Option<ID3D11Device> = None;
    let mut ctx: Option<ID3D11DeviceContext> = None;
    D3D11CreateDevice(
        &adapter.cast::<IDXGIAdapter>()?,
        D3D_DRIVER_TYPE_UNKNOWN,
        Default::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut dev),
        None,
        Some(&mut ctx),
    )?;
    let (dev, ctx) = (dev.unwrap(), ctx.unwrap());
    if let Ok(mt) = dev.cast::<ID3D11Multithread>() {
        let _ = mt.SetMultithreadProtected(true);
    }

    let mut screens = Vec::new();
    for (out, rect) in &outputs {
        let dup = at(out.DuplicateOutput(&dev), "duplicate display")?;
        screens.push(Screen { dup, rect: *rect, desk: None });
    }
    // H.264 needs even dimensions.
    let area = s.area.intersect(&crate::capture::virtual_screen()).unwrap_or(s.area);
    let area = Rect { x: area.x, y: area.y, w: area.w & !1, h: area.h & !1 };
    let (w, h) = (area.w as u32, area.h as u32);
    // Parts of the area no display of this GPU covers are filled black.
    let covered: i64 = screens.iter().filter_map(|sc| sc.rect.intersect(&area)).map(|i| i.w as i64 * i.h as i64).sum();
    let gaps = covered < area.w as i64 * area.h as i64;
    if w < 16 || h < 16 {
        return Err(windows::core::Error::new(E_INVALID, "The recording area is too small"));
    }

    // Audio sources (either may be missing, e.g. no mic plugged in).
    let mut sys = if s.system_audio { open_source(true).ok() } else { None };
    let mut mic = if s.mic { open_source(false).ok() } else { None };
    let with_audio = sys.is_some() || mic.is_some();

    let path = HSTRING::from(s.out.as_os_str());
    if let Some(dir) = s.out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut token = 0u32;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
    let manager = manager.unwrap();
    manager.ResetDevice(&dev, token)?;
    let writer = match make_writer(&path, s, w, h, Some(&manager), with_audio) {
        Ok(wr) => wr,
        Err(e) => {
            crate::overlay::log(&format!("gpu encoder path failed ({e}), using cpu path"));
            at(make_writer(&path, s, w, h, None, with_audio), "create encoder")?
        }
    };

    let pool: Vec<ID3D11Texture2D> = at((0..6).map(|_| texture(&dev, w, h, false)).collect::<Result<_>>(), "frame pool")?;
    let rtvs: Vec<Option<ID3D11RenderTargetView>> = pool
        .iter()
        .map(|t| {
            let mut v = None;
            if gaps {
                let _ = dev.CreateRenderTargetView(t, None, Some(&mut v));
            }
            v
        })
        .collect();
    let staging = if writer.gpu { None } else { Some(texture(&dev, w, h, true)?) };
    let mut cursor = CursorCache { handle: 0, hot: (0, 0) };
    let interval = HNS / s.fps.max(1) as i64;
    let mut frame_no: i64 = 0;
    let mut audio_frames: i64 = 0;
    log(&format!("rec setup {:.0} ms, {}x{} on {} display(s), gpu={} audio={} (sys {}, mic {})", t_setup.elapsed().as_secs_f64() * 1000.0, w, h, screens.len(), writer.gpu, with_audio, sys.is_some(), mic.is_some()));
    let (mut n_video, mut n_audio, mut n_acq, mut n_nodesk) = (0u32, 0u32, 0u32, 0u32);
    // Wait for the countdown, keeping the audio buffers and desktop copy fresh.
    ctl.ready.store(true, Ordering::SeqCst);
    while !ctl.go.load(Ordering::SeqCst) && !ctl.stop.load(Ordering::SeqCst) {
        if let Some(src) = sys.as_mut() {
            drain(src);
            src.queue.clear();
        }
        if let Some(src) = mic.as_mut() {
            drain(src);
            src.queue.clear();
        }
        poll(&mut screens, &dev, &ctx, 10)?;
    }
    if ctl.stop.load(Ordering::SeqCst) && !ctl.go.load(Ordering::SeqCst) {
        // Cancelled before it started: no file.
        drop(writer);
        return Err(windows::core::Error::new(E_INVALID, "cancelled"));
    }
    ctl.restart_clock();
    while !ctl.stop.load(Ordering::SeqCst) {
        // Audio: always drain (so buffers don't overflow), only write while recording.
        if let Some(src) = sys.as_mut() {
            drain(src);
        }
        if let Some(src) = mic.as_mut() {
            drain(src);
        }
        let paused = ctl.paused.load(Ordering::SeqCst);
        let now = ctl.elapsed().as_nanos() as i64 / 100;
        if paused {
            if let Some(src) = sys.as_mut() {
                src.queue.clear();
            }
            if let Some(src) = mic.as_mut() {
                src.queue.clear();
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        if let Some(stream) = writer.audio {
            let target = now * RATE as i64 / HNS;
            let n = (target - audio_frames).max(0) as usize;
            if n >= 480 {
                let muted = ctl.mic_muted.load(Ordering::SeqCst);
                let mut pcm: Vec<i16> = Vec::with_capacity(n * 2);
                for _ in 0..n * 2 {
                    let a = sys.as_mut().and_then(|q| q.queue.pop_front()).unwrap_or(0.0);
                    let b = mic.as_mut().and_then(|q| q.queue.pop_front()).unwrap_or(0.0);
                    let v = (a + if muted { 0.0 } else { b }).clamp(-1.0, 1.0);
                    pcm.push((v * 32767.0) as i16);
                }
                let bytes = (pcm.len() * 2) as u32;
                let buf = MFCreateMemoryBuffer(bytes)?;
                let mut ptr: *mut u8 = std::ptr::null_mut();
                buf.Lock(&mut ptr, None, None)?;
                std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, ptr, bytes as usize);
                buf.Unlock()?;
                buf.SetCurrentLength(bytes)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buf)?;
                sample.SetSampleTime(audio_frames * HNS / RATE as i64)?;
                sample.SetSampleDuration(n as i64 * HNS / RATE as i64)?;
                at(writer.sink.WriteSample(stream, &sample), "audio write")?;
                audio_frames += n as i64;
                n_audio += 1;
            }
        }

        // Video: one frame per interval, repeating the last image when nothing changed.
        let due = frame_no * interval;
        if now < due {
            let wait_ms = (((due - now) / 10_000) as u32).clamp(1, 15);
            n_acq += poll(&mut screens, &dev, &ctx, wait_ms)?;
            continue;
        }
        if now - due > interval * 3 {
            // Fell behind (machine busy): skip ahead instead of bunching frames.
            frame_no = now / interval;
        }
        if screens.iter().all(|sc| sc.desk.is_none()) {
            n_nodesk += 1;
            frame_no += 1;
            continue;
        }
        let slot = (frame_no as usize) % pool.len();
        let tex = &pool[slot];
        if let Some(rtv) = &rtvs[slot] {
            ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]);
        }
        // Stitch each display's part of the area into the frame.
        for sc in &screens {
            let (Some(src), Some(i)) = (sc.desk.as_ref(), sc.rect.intersect(&area)) else { continue };
            let bx = D3D11_BOX {
                left: (i.x - sc.rect.x) as u32,
                top: (i.y - sc.rect.y) as u32,
                front: 0,
                right: (i.right() - sc.rect.x) as u32,
                bottom: (i.bottom() - sc.rect.y) as u32,
                back: 1,
            };
            ctx.CopySubresourceRegion(tex, 0, (i.x - area.x) as u32, (i.y - area.y) as u32, 0, src, 0, Some(&bx));
        }
        if s.cursor {
            draw_cursor(tex, &area, &mut cursor);
        }
        let buf = if let Some(st) = &staging {
            // CPU fallback when the encoder can't take GPU textures.
            ctx.CopyResource(st, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let len = w * h * 4;
            let b = MFCreateMemoryBuffer(len)?;
            let mut p: *mut u8 = std::ptr::null_mut();
            b.Lock(&mut p, None, None)?;
            for row in 0..h as usize {
                std::ptr::copy_nonoverlapping((m.pData as *const u8).add(row * m.RowPitch as usize), p.add(row * w as usize * 4), w as usize * 4);
            }
            b.Unlock()?;
            ctx.Unmap(st, 0);
            b.SetCurrentLength(len)?;
            b
        } else {
            let b = at(MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, tex, 0, false), "frame buffer")?;
            if let Ok(b2) = b.cast::<IMF2DBuffer>() {
                if let Ok(len) = b2.GetContiguousLength() {
                    b.SetCurrentLength(len)?;
                }
            }
            b
        };
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        sample.SetSampleTime(frame_no * interval)?;
        sample.SetSampleDuration(interval)?;
        at(writer.sink.WriteSample(writer.video, &sample), "video write")?;
        frame_no += 1;
        n_video += 1;
        ctl.live.store(true, Ordering::SeqCst);
    }
    log(&format!("rec loop: {n_video} video, {n_audio} audio, {n_acq} desktop frames, {n_nodesk} early ticks, {:.1}s", ctl.elapsed().as_secs_f64()));

    if let Some(src) = &sys {
        let _ = src.client.Stop();
    }
    if let Some(src) = &mic {
        let _ = src.client.Stop();
    }
    at(writer.sink.Finalize(), "finalize")?;
    Ok(())
}

/// Add which step failed to an error, so failures say where they happened.
fn at<T>(r: Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| windows::core::Error::new(e.code(), format!("{what}: {}", e.message())))
}

const E_INVALID: windows::core::HRESULT = windows::core::HRESULT(0x80070057u32 as i32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_excludes_pauses() {
        let c = Control::new();
        std::thread::sleep(Duration::from_millis(30));
        c.set_paused(true);
        let a = c.elapsed();
        std::thread::sleep(Duration::from_millis(40));
        assert!(c.elapsed() - a < Duration::from_millis(5));
        c.set_paused(false);
        assert!(c.elapsed() >= a);
    }
}
