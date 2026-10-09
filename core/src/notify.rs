//! The after-capture toast, bottom right above the taskbar.
//!
//! Screenshot: thumbnail, "Copied and saved", path, and Edit / Copy text /
//! Show in folder / Delete. Recording: "Recording saved to folder" with
//! Edit (trim) / Show in folder / Delete. It stays 4 seconds, and hovering
//! pauses the timer. Clicking the thumbnail or Edit opens the editor; nothing
//! opens on its own. The toast is excluded from capture.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_INTERPOLATION_MODE_LINEAR};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::gfx::{rf, Gfx, Surface};
use crate::output::Image;
use crate::recorder;
use crate::theme::{self, Palette};

const CLASS: windows::core::PCWSTR = w!("FastSnipToast");
const TICK: usize = 9;
const SHOW_FOR: Duration = Duration::from_secs(4);
const W: f32 = 340.0;
const H: f32 = 92.0;
pub const WM_TEXT_COPIED: u32 = WM_APP + 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Edit,
    CopyText,
    Folder,
    Delete,
}

enum Kind {
    Shot,
    Video { seconds: f64 },
    Message,
}

struct Toast {
    hwnd: HWND,
    surf: Surface,
    kind: Kind,
    title: String,
    path: Option<PathBuf>,
    thumb: Option<ID2D1Bitmap1>,
    thumb_size: (u32, u32),
    image: Option<std::sync::Arc<Image>>,
    left: Duration,
    last: Instant,
    hover: Option<Act>,
    hovering: bool,
    scale: f32,
}

struct Notify {
    gfx: Option<Gfx>,
    toast: Option<Toast>,
    palette: Palette,
    enabled: bool,
    main: HWND,
}

thread_local! {
    static N: RefCell<Option<Notify>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Notify) -> R) -> Option<R> {
    N.with(|c| c.try_borrow_mut().ok().and_then(|mut n| n.as_mut().map(f)))
}

pub fn install(main: HWND, cfg: &Config) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
    let n = Notify { gfx: None, toast: None, palette: theme::palette(&cfg.look.accent, &cfg.look.theme), enabled: cfg.saving.show_toast, main };
    N.with(|c| *c.borrow_mut() = Some(n));
}

pub fn apply_config(cfg: &Config) {
    with(|n| {
        n.palette = theme::palette(&cfg.look.accent, &cfg.look.theme);
        n.enabled = cfg.saving.show_toast;
    });
}

pub fn screenshot_saved(img: std::sync::Arc<Image>, path: &Path) {
    show(Kind::Shot, "Copied and saved".into(), Some(path.to_path_buf()), Some(img));
}

pub fn recording_saved(d: &recorder::Done) {
    match (&d.path, &d.error) {
        (Some(p), None) => show(Kind::Video { seconds: d.seconds }, "Recording saved to folder".into(), Some(p.clone()), None),
        (_, Some(e)) if e != "cancelled" => show(Kind::Message, format!("Recording failed: {e}"), None, None),
        _ => {}
    }
}

pub fn is_showing() -> bool {
    with(|n| n.toast.is_some()).unwrap_or(false)
}

pub fn message(text: &str) {
    show(Kind::Message, text.to_string(), None, None);
}

fn show(kind: Kind, title: String, path: Option<PathBuf>, image: Option<std::sync::Arc<Image>>) {
    with(|n| {
        if !n.enabled && !matches!(kind, Kind::Message) {
            return;
        }
        if let Some(t) = n.toast.take() {
            unsafe {
                let _ = DestroyWindow(t.hwnd);
            }
        }
        if n.gfx.is_none() {
            n.gfx = Gfx::new().ok();
        }
        let Some(g) = n.gfx.as_ref() else { return };
        // Bottom right of the display with the mouse, above the taskbar.
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mons = crate::capture::monitors();
        let Some(m) = mons.iter().find(|m| m.rect.contains(pt.x, pt.y)).or(mons.first()) else { return };
        let sc = m.scale();
        let (w, h) = ((W * sc) as i32, (H * sc) as i32);
        let x = m.work.right() - w - (14.0 * sc) as i32;
        let y = m.work.bottom() - h - (14.0 * sc) as i32;
        let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                &HSTRING::from("FastSnip"),
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
        };
        let Ok(hwnd) = hwnd else { return };
        unsafe {
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, (24.0 * sc) as i32, (24.0 * sc) as i32);
            SetWindowRgn(hwnd, Some(rgn), false);
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
        let Ok(surf) = g.surface(hwnd, w as u32, h as u32) else { return };
        let (thumb, thumb_size) = match &image {
            Some(img) => (g.bitmap(img.bgra.as_ptr(), img.w, img.h, img.w * 4).ok(), (img.w, img.h)),
            None => (None, (0, 0)),
        };
        n.toast = Some(Toast {
            hwnd,
            surf,
            kind,
            title,
            path,
            thumb,
            thumb_size,
            image,
            left: SHOW_FOR,
            last: Instant::now(),
            hover: None,
            hovering: false,
            scale: sc,
        });
        draw(n);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), TICK, 50, None);
        }
    });
}

fn actions(kind: &Kind) -> Vec<(Act, &'static str)> {
    match kind {
        Kind::Shot => vec![(Act::Edit, "pen-line"), (Act::CopyText, "scan-text"), (Act::Folder, "folder-open"), (Act::Delete, "trash-2")],
        Kind::Video { .. } => vec![(Act::Edit, "scissors"), (Act::Folder, "folder-open"), (Act::Delete, "trash-2")],
        Kind::Message => vec![],
    }
}

fn layout(t: &Toast) -> (D2D_RECT_F, Vec<(Act, &'static str, D2D_RECT_F)>) {
    let s = t.scale;
    let thumb = rf(12.0 * s, 12.0 * s, 92.0 * s, 62.0 * s);
    let x0 = if matches!(t.kind, Kind::Message) { 16.0 * s } else { 116.0 * s };
    let btns = actions(&t.kind)
        .into_iter()
        .enumerate()
        .map(|(i, (a, ic))| (a, ic, rf(x0 - 6.0 * s + i as f32 * 32.0 * s, 48.0 * s, 28.0 * s, 28.0 * s)))
        .collect();
    (thumb, btns)
}

fn draw(n: &mut Notify) {
    let p = n.palette;
    let Some(g) = n.gfx.as_mut() else { return };
    let Some(t) = n.toast.as_mut() else { return };
    if g.begin(&mut t.surf).is_err() {
        return;
    }
    let s = t.scale;
    let (w, h) = (W * s, H * s);
    g.fill(rf(0.0, 0.0, w, h), p.bar);
    g.stroke_round(rf(0.5, 0.5, w - 1.0, h - 1.0), 12.0 * s, p.bar_line, 1.0);
    let (thumb, btns) = layout(t);
    let x0 = if matches!(t.kind, Kind::Message) { 16.0 * s } else { 116.0 * s };
    if !matches!(t.kind, Kind::Message) {
        g.fill_round(thumb, 6.0 * s, p.hover);
        if let Some(bmp) = &t.thumb {
            // Fit the image inside the thumbnail box.
            let (iw, ih) = (t.thumb_size.0 as f32, t.thumb_size.1 as f32);
            let k = ((thumb.right - thumb.left) / iw).min((thumb.bottom - thumb.top) / ih);
            let (dw, dh) = (iw * k, ih * k);
            let dx = thumb.left + ((thumb.right - thumb.left) - dw) / 2.0;
            let dy = thumb.top + ((thumb.bottom - thumb.top) - dh) / 2.0;
            unsafe {
                g.dc.DrawBitmap(bmp, Some(&rf(dx, dy, dw, dh)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        } else if let Kind::Video { seconds } = t.kind {
            g.icon("video", (thumb.left + thumb.right) / 2.0 - 12.0 * s, (thumb.top + thumb.bottom) / 2.0 - 12.0 * s, 24.0 * s, p.muted);
            let d = format!("{}:{:02}", seconds as u64 / 60, seconds as u64 % 60);
            let f = g.fonts(s).key.clone();
            let (dw, _) = g.text_size(&d, &f);
            g.text(&d, &f, thumb.right - dw - 6.0 * s, thumb.bottom - 16.0 * s, p.fg);
        }
    }
    let bold = g.fonts(s).label_bold.clone();
    let mono = g.fonts(s).key.clone();
    g.icon("circle-check", x0, 13.0 * s, 15.0 * s, theme::rgb(if p.dark { 0x5fd08f } else { 0x1d8a4e }));
    g.text(&t.title, &bold, x0 + 21.0 * s, 11.0 * s, p.fg);
    if let Some(path) = &t.path {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let folder = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut line = format!("{folder}\\{name}");
        if line.chars().count() > 38 {
            line = format!("…{}", line.chars().rev().take(37).collect::<Vec<_>>().into_iter().rev().collect::<String>());
        }
        g.text(&line, &mono, x0, 31.0 * s, p.muted);
    }
    for (a, ic, r) in &btns {
        if t.hover == Some(*a) {
            g.fill_round(*r, 7.0 * s, p.hover);
        }
        let c = if *a == Act::Delete { p.rec } else { p.fg };
        g.icon(ic, r.left + 6.0 * s, r.top + 6.0 * s, 16.0 * s, c);
    }
    // Time left.
    let frac = t.left.as_secs_f32() / SHOW_FOR.as_secs_f32();
    g.fill(rf(0.0, h - 3.0 * s, w, 3.0 * s), p.bar_line);
    g.fill(rf(0.0, h - 3.0 * s, w * frac, 3.0 * s), p.accent);
    let _ = g.end(&t.surf);
}

fn hit(x: i32, y: i32) -> (Option<Act>, bool) {
    with(|n| {
        let Some(t) = n.toast.as_ref() else { return (None, false) };
        let (thumb, btns) = layout(t);
        let (fx, fy) = (x as f32, y as f32);
        let inside = |r: &D2D_RECT_F| fx >= r.left && fx < r.right && fy >= r.top && fy < r.bottom;
        let act = btns.iter().find(|(_, _, r)| inside(r)).map(|(a, _, _)| *a);
        (act, !matches!(t.kind, Kind::Message) && inside(&thumb))
    })
    .unwrap_or((None, false))
}

fn close() {
    with(|n| {
        if let Some(t) = n.toast.take() {
            unsafe {
                let _ = DestroyWindow(t.hwnd);
            }
        }
    });
}

fn run(a: Act) {
    let info = with(|n| n.toast.as_ref().map(|t| (t.path.clone(), t.image.clone(), n.main))).flatten();
    let Some((path, image, main)) = info else { return };
    match a {
        Act::Edit => {
            if let Some(p) = &path {
                crate::overlay::open_app_with(&["--edit", &p.to_string_lossy()]);
            }
            close();
        }
        Act::Folder => {
            if let Some(p) = &path {
                let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
            }
            close();
        }
        Act::Delete => {
            if let Some(p) = &path {
                recycle(p);
            }
            close();
        }
        Act::CopyText => {
            let Some(img) = image else { return };
            let target = main.0 as isize;
            std::thread::spawn(move || {
                let job = crate::ocr::Job { seq: 0, kind: crate::ocr::Kind::Region, origin: (0, 0), w: img.w, h: img.h, bgra: img.bgra.clone(), upscale: (img.w * img.h) <= 600_000 };
                let mut words = crate::ocr::read_now(&job);
                crate::ocr::sort_reading(&mut words);
                let text = crate::ocr::join(&words, true);
                let msg = Box::new((text, words.len()));
                unsafe {
                    let _ = PostMessageW(Some(HWND(target as *mut _)), WM_TEXT_COPIED, WPARAM(0), LPARAM(Box::into_raw(msg) as isize));
                }
            });
        }
    }
}

/// Main thread: OCR from the toast finished.
pub fn on_text(owner: HWND, text: String, words: usize) {
    if words == 0 {
        message("No text found in that screenshot");
        return;
    }
    crate::output::clipboard_set_text(owner, &text);
    message(&format!("Copied {words} {}", if words == 1 { "word" } else { "words" }));
}

/// Move a file to the Recycle Bin (so Delete can be undone).
fn recycle(p: &Path) {
    let mut from: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().collect();
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    unsafe {
        let _ = SHFileOperationW(&mut op);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_TIMER if wp.0 == TICK => {
            let expired = with(|n| {
                let Some(t) = n.toast.as_mut() else { return false };
                let now = Instant::now();
                if !t.hovering {
                    t.left = t.left.saturating_sub(now - t.last);
                }
                t.last = now;
                let done = t.left.is_zero();
                draw(n);
                done
            })
            .unwrap_or(false);
            if expired {
                close();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (a, _) = hit(x, y);
            with(|n| {
                if let Some(t) = n.toast.as_mut() {
                    t.hover = a;
                    t.hovering = true;
                }
                draw(n);
            });
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            with(|n| {
                if let Some(t) = n.toast.as_mut() {
                    t.hover = None;
                    t.hovering = false;
                }
                draw(n);
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            match hit(x, y) {
                (Some(a), _) => run(a),
                (None, true) => run(Act::Edit),
                _ => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            close();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            with(draw);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
