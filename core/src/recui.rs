//! Recording UI: countdown, the recording pill (time, mic, pause, stop) and
//! the red frame around the area. All of these windows use
//! WDA_EXCLUDEFROMCAPTURE, so they never appear in the video.
//!
//! Flow: an area is picked -> `prepare` starts the recorder's slow setup
//! (hardware encoder, ~1.5-2 s) right away -> `begin` runs the countdown,
//! which hides that setup -> the recorder is told to go.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::{self, Rect};
use crate::config::Config;
use crate::gfx::{rf, Gfx, Surface};
use crate::output;
use crate::recorder::{self, Control};
use crate::theme::{self, Palette};

const PILL_CLASS: windows::core::PCWSTR = w!("FastSnipRecPill");
const FRAME_CLASS: windows::core::PCWSTR = w!("FastSnipRecFrame");
const TICK: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PillBtn {
    Mic,
    Pause,
    Stop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Recorder is setting up, waiting for the user to press Start.
    Prepared,
    Countdown,
    Recording,
    Saving,
}

struct Session {
    ctl: Arc<Control>,
    area: Rect,
    stage: Stage,
    countdown_end: Option<Instant>,
    countdown_secs: u32,
    pill: Option<(HWND, Surface)>,
    frames: Vec<HWND>,
    scale: f32,
    hover: Option<PillBtn>,
    /// The "turn on microphone access" toast was shown for this recording.
    warned: bool,
}

struct RecUi {
    gfx: Option<Gfx>,
    session: Option<Session>,
    main: HWND,
    palette: Palette,
    cfg: Config,
}

thread_local! {
    static UI: RefCell<Option<RecUi>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut RecUi) -> R) -> Option<R> {
    UI.with(|c| c.try_borrow_mut().ok().and_then(|mut u| u.as_mut().map(f)))
}

pub fn install(main: HWND, cfg: &Config) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(pill_proc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PILL_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let red = CreateSolidBrush(COLORREF(0x003636d9)); // #D93636 as 0x00BBGGRR
        let wf = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(frame_proc),
            hInstance: hinst.into(),
            hbrBackground: red,
            lpszClassName: FRAME_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wf);
    }
    let ui = RecUi { gfx: None, session: None, main, palette: theme::palette(&cfg.look.accent, &cfg.look.theme), cfg: cfg.clone() };
    UI.with(|c| *c.borrow_mut() = Some(ui));
}

pub fn apply_config(cfg: &Config) {
    with(|u| {
        u.cfg = cfg.clone();
        u.palette = theme::palette(&cfg.look.accent, &cfg.look.theme);
    });
}

pub fn is_busy() -> bool {
    with(|u| u.session.as_ref().map(|s| s.stage != Stage::Prepared).unwrap_or(false)).unwrap_or(false)
}

pub fn is_prepared() -> bool {
    with(|u| u.session.as_ref().map(|s| s.stage == Stage::Prepared).unwrap_or(false)).unwrap_or(false)
}

/// Start the recorder's setup for `area` now; nothing is captured until `begin`.
pub fn prepare(area: Rect, mic: bool, system_audio: bool) {
    with(|u| {
        if let Some(old) = u.session.take() {
            old.ctl.stop.store(true, Ordering::SeqCst);
        }
        let rc = &u.cfg.recording;
        let dir = output::recordings_dir(&u.cfg.saving.recordings_dir);
        let out: PathBuf = output::unique_path(&dir, &output::timestamp_name("Screen Recording", "mp4"));
        let settings = recorder::Settings {
            area,
            fps: rc.fps.clamp(15, 120),
            mbps: rc.mbps(area.w, area.h),
            mic,
            system_audio,
            cursor: rc.show_cursor,
            out,
        };
        let ctl = recorder::start(settings, u.main);
        let scale = capture::monitors()
            .iter()
            .find(|m| m.rect.contains(area.x + area.w / 2, area.y + area.h / 2))
            .map(|m| m.scale())
            .unwrap_or(1.0);
        u.session = Some(Session {
            ctl,
            area,
            stage: Stage::Prepared,
            countdown_end: None,
            countdown_secs: rc.countdown,
            pill: None,
            frames: Vec::new(),
            scale,
            hover: None,
            warned: false,
        });
    });
}

/// Throw away a prepared (not started) recording.
pub fn cancel_prepared() {
    with(|u| {
        if u.session.as_ref().map(|s| s.stage == Stage::Prepared).unwrap_or(false) {
            let s = u.session.take().unwrap();
            s.ctl.stop.store(true, Ordering::SeqCst);
        }
    });
}

/// Show the frame and pill, run the countdown, then record.
pub fn begin() {
    with(|u| {
        if u.gfx.is_none() {
            u.gfx = Gfx::new().ok();
        }
        let Some(s) = u.session.as_mut() else { return };
        if s.stage != Stage::Prepared {
            return;
        }
        s.stage = Stage::Countdown;
        crate::tray::set_recording(u.main, true);
        s.countdown_end = Some(Instant::now() + std::time::Duration::from_secs(s.countdown_secs as u64));
        s.frames = make_frames(&s.area, s.scale);
        let pill = make_pill(&s.area, s.scale);
        if let (Some(h), Some(g)) = (pill, u.gfx.as_ref()) {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetClientRect(h, &mut rc);
            }
            if let Ok(surf) = g.surface(h, rc.right as u32, rc.bottom as u32) {
                s.pill = Some((h, surf));
            }
            unsafe {
                SetTimer(Some(h), TICK, 50, None);
            }
        }
    });
    tick();
}

/// Stop button, tray, or the record shortcut.
pub fn stop() {
    with(|u| {
        if let Some(s) = u.session.as_mut() {
            if s.stage == Stage::Countdown {
                // Stopped during the countdown: nothing was recorded.
                s.ctl.stop.store(true, Ordering::SeqCst);
                let s = u.session.take().unwrap();
                destroy(s);
                crate::tray::set_recording(u.main, false);
                return;
            }
            s.ctl.stop.store(true, Ordering::SeqCst);
            s.stage = Stage::Saving;
        }
    });
    tick();
}

/// The recorder finished writing.
pub fn on_done(d: recorder::Done) {
    with(|u| {
        if let Some(s) = u.session.take() {
            destroy(s);
        }
        crate::tray::set_recording(u.main, false);
    });
    match (&d.path, &d.error) {
        (Some(p), None) => crate::overlay::log(&format!("recording saved: {} ({:.1} s)", p.display(), d.seconds)),
        (_, Some(e)) if e != "cancelled" => crate::overlay::log(&format!("recording failed: {e}")),
        _ => {}
    }
    crate::notify::recording_saved(&d);
}

fn destroy(s: Session) {
    unsafe {
        for f in s.frames {
            let _ = DestroyWindow(f);
        }
        if let Some((h, surf)) = s.pill {
            drop(surf);
            let _ = DestroyWindow(h);
        }
    }
}

fn exclude(h: HWND) {
    unsafe {
        let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
    }
}

fn make_frames(a: &Rect, scale: f32) -> Vec<HWND> {
    let mon = capture::monitors().into_iter().find(|m| m.rect.contains(a.x + a.w / 2, a.y + a.h / 2));
    // No frame for a full-display recording: there is no room around it.
    if mon.map(|m| m.rect == *a).unwrap_or(false) {
        return Vec::new();
    }
    let t = (2.0 * scale).round().max(2.0) as i32;
    let rects = [
        (a.x - t, a.y - t, a.w + 2 * t, t),
        (a.x - t, a.bottom(), a.w + 2 * t, t),
        (a.x - t, a.y, t, a.h),
        (a.right(), a.y, t, a.h),
    ];
    let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
    rects
        .iter()
        .filter_map(|&(x, y, w, h)| unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
                FRAME_CLASS,
                &HSTRING::from("FastSnip recording frame"),
                WS_POPUP,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .ok()?;
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            exclude(hwnd);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(hwnd)
        })
        .collect()
}

const PILL_W: f32 = 200.0;
const PILL_H: f32 = 40.0;

fn make_pill(a: &Rect, scale: f32) -> Option<HWND> {
    let (w, h) = ((PILL_W * scale) as i32, (PILL_H * scale) as i32);
    let mon = capture::monitors().into_iter().find(|m| m.rect.contains(a.x + a.w / 2, a.y + a.h / 2))?;
    let gap = (10.0 * scale) as i32;
    let x = (a.x + a.w / 2 - w / 2).clamp(mon.rect.x + 4, mon.rect.right() - w - 4);
    // Above the area if there's room, then below, else inside at the top (it's excluded anyway).
    let y = if a.y - h - gap >= mon.rect.y {
        a.y - h - gap
    } else if a.bottom() + gap + h <= mon.work.bottom() {
        a.bottom() + gap
    } else {
        mon.rect.y + gap
    };
    let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            PILL_CLASS,
            &HSTRING::from("FastSnip recording"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, (24.0 * scale) as i32, (24.0 * scale) as i32);
        SetWindowRgn(hwnd, Some(rgn), false);
        exclude(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Some(hwnd)
    }
}

fn pill_buttons(scale: f32) -> [(PillBtn, f32); 3] {
    let x0 = PILL_W - 4.0 - 34.0 * 3.0;
    [(PillBtn::Mic, x0), (PillBtn::Pause, x0 + 34.0), (PillBtn::Stop, x0 + 68.0)].map(|(b, x)| (b, x * scale))
}

/// Advance the countdown and redraw the pill.
fn tick() {
    with(|u| {
        let Some(s) = u.session.as_mut() else { return };
        if s.stage == Stage::Countdown {
            let done = s.countdown_end.map(|e| Instant::now() >= e).unwrap_or(true);
            if done && s.ctl.ready.load(Ordering::SeqCst) {
                s.ctl.go.store(true, Ordering::SeqCst);
                s.stage = Stage::Recording;
            }
        }
        // Tell the user right away if Windows is blocking the mic (the toast stays out of the video).
        if s.stage == Stage::Recording && !s.warned && s.ctl.mic_blocked.load(Ordering::SeqCst) {
            s.warned = true;
            crate::notify::mic_permission(false);
        }
        draw(u);
    });
}

fn fmt_time(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

fn draw(u: &mut RecUi) {
    let p = u.palette;
    let Some(g) = u.gfx.as_mut() else { return };
    let Some(s) = u.session.as_mut() else { return };
    let sc = s.scale;
    let Some((_, surf)) = s.pill.as_mut() else { return };
    if g.begin(surf).is_err() {
        return;
    }
    let (w, h) = (PILL_W * sc, PILL_H * sc);
    g.fill(rf(0.0, 0.0, w, h), p.bar);
    g.stroke_round(rf(0.5, 0.5, w - 1.0, h - 1.0), 12.0 * sc, p.bar_line, 1.0);
    let label = g.fonts(sc).label.clone();
    let mono = g.fonts(sc).badge.clone();
    let cy = h / 2.0;
    let paused = s.ctl.paused.load(Ordering::SeqCst);
    let text: String = match s.stage {
        Stage::Countdown => {
            let left = s.countdown_end.map(|e| e.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32).unwrap_or(0);
            if left > 0 {
                format!("Starting in {left}")
            } else {
                "Starting…".into()
            }
        }
        Stage::Saving => "Saving…".into(),
        _ if !s.ctl.live.load(Ordering::SeqCst) => "Starting…".into(),
        _ => fmt_time(s.ctl.elapsed().as_secs()),
    };
    // Dot: red and blinking while recording, steady amber when paused.
    let blink = (unsafe { GetTickCount() } / 600) % 2 == 0;
    let on_air = s.stage == Stage::Recording && s.ctl.live.load(Ordering::SeqCst);
    let dot = if paused { theme::rgb(0xf59f00) } else { p.rec };
    if !on_air || paused || blink {
        let r = 4.5 * sc;
        g.fill_round(rf(14.0 * sc - r, cy - r, r * 2.0, r * 2.0), r, dot);
    }
    let f = if on_air { &mono } else { &label };
    let (_, th) = g.text_size(&text, f);
    g.text(&text, f, 26.0 * sc, cy - th / 2.0, p.fg);
    let muted = s.ctl.mic_muted.load(Ordering::SeqCst);
    for (b, x) in pill_buttons(sc) {
        let r = rf(x, (h - 32.0 * sc) / 2.0, 32.0 * sc, 32.0 * sc);
        if s.hover == Some(b) {
            g.fill_round(r, 8.0 * sc, p.hover);
        }
        let (icon, c) = match b {
            PillBtn::Mic => (if muted { "mic-off" } else { "mic" }, if muted { p.muted } else { p.fg }),
            PillBtn::Pause => (if paused { "circle-dot" } else { "pause" }, p.fg),
            PillBtn::Stop => ("square", p.rec),
        };
        g.icon(icon, r.left + 7.0 * sc, r.top + 7.0 * sc, 18.0 * sc, c);
    }
    let _ = g.end(surf);
}

fn pill_hit(x: i32, y: i32) -> Option<PillBtn> {
    with(|u| {
        let s = u.session.as_ref()?;
        let sc = s.scale;
        let (fx, fy) = (x as f32, y as f32);
        let top = (PILL_H * sc - 32.0 * sc) / 2.0;
        pill_buttons(sc)
            .into_iter()
            .find(|(_, bx)| fx >= *bx && fx < bx + 32.0 * sc && fy >= top && fy < top + 32.0 * sc)
            .map(|(b, _)| b)
    })
    .flatten()
}

unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_TIMER if wp.0 == TICK => {
            tick();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let b = pill_hit(x, y);
            with(|u| {
                if let Some(s) = u.session.as_mut() {
                    s.hover = b;
                }
            });
            let mut tme = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut tme);
            tick();
            LRESULT(0)
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            with(|u| {
                if let Some(s) = u.session.as_mut() {
                    s.hover = None;
                }
            });
            tick();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            match pill_hit(x, y) {
                None => {
                    // Drag the pill out of the way.
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
                }
                Some(PillBtn::Stop) => stop(),
                Some(PillBtn::Pause) => {
                    with(|u| {
                        if let Some(s) = &u.session {
                            if s.stage == Stage::Recording {
                                let p = s.ctl.paused.load(Ordering::SeqCst);
                                s.ctl.set_paused(!p);
                            }
                        }
                    });
                    tick();
                }
                Some(PillBtn::Mic) => {
                    with(|u| {
                        if let Some(s) = &u.session {
                            let m = s.ctl.mic_muted.load(Ordering::SeqCst);
                            s.ctl.mic_muted.store(!m, Ordering::SeqCst);
                        }
                    });
                    tick();
                }
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            tick();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

use windows::Win32::System::SystemInformation::GetTickCount;

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}
