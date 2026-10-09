//! The frozen-screen overlay: one borderless topmost window per monitor that
//! shows the grabbed frame, the toolbar (layout B from the design spec, with
//! key hints), the selection for each capture shape, and the loupe.
//!
//! The toolbar is drawn into the overlay, never onto the real screen, and
//! every capture is cut from the frozen frame, so it can't show up in a shot.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use windows::core::{w, Interface, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Matrix3x2;

use crate::capture::{self, Frame, Monitor, Rect, WinInfo};
use crate::config::Config;
use crate::dup::Dup;
use crate::element::{self, Finder};
use crate::gfx::{rf, Gfx, Surface};
use crate::hotkey::Action;
use crate::livetext::{search_url, BarBtn, LiveText};
use crate::ocr::{self, Reader, Word};
use crate::output::{self, Image};
use crate::theme::{self, with_alpha, Palette};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    Snip,
    Record,
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Rect,
    Window,
    Element,
    Free,
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Intent(Intent),
    Shape(Shape),
    Mic,
    Audio,
    OpenApp,
    Close,
}

const ANIM_TIMER: usize = 1;
const DIM_MS: f32 = 90.0;
const BAR_MS: f32 = 140.0;

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Ease out with a small overshoot, for the toolbar "pop".
fn ease_out_back(t: f32) -> f32 {
    let c1 = 1.0;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

const SHAPES: [(Shape, &str, &str); 5] = [
    (Shape::Rect, "square-dashed", "1"),
    (Shape::Window, "app-window", "2"),
    (Shape::Element, "scan", "3"),
    (Shape::Free, "lasso", "4"),
    (Shape::Full, "monitor", "5"),
];

struct Drag {
    start: (i32, i32),
    pts: Vec<(i32, i32)>,
    mon: usize,
}

struct OvWin {
    hwnd: HWND,
    mon: Monitor,
    surf: Option<Surface>,
    frozen: Option<ID2D1Bitmap1>,
}

/// What a click or drag selected.
enum Target {
    Area(Rect),
    Shape(Vec<(i32, i32)>),
}

pub struct Overlay {
    gfx: Gfx,
    dup: Option<Dup>,
    wins: Vec<OvWin>,
    pub visible: bool,
    frame: Option<Frame>,
    snapshot: Vec<WinInfo>,
    intent: Intent,
    shape: Shape,
    mouse: (i32, i32),
    drag: Option<Drag>,
    hover: Option<Btn>,
    pressed: Option<Btn>,
    tb_mon: usize,
    full_mon: Option<usize>,
    element: Option<(Rect, String)>,
    elem_seq: u64,
    finder: Finder,
    mic: bool,
    audio: bool,
    palette: Palette,
    magnifier: bool,
    animations: bool,
    anim_speed: f32,
    anim_t0: Option<Instant>,
    shots_dir: PathBuf,
    dash: Option<ID2D1StrokeStyle>,
    notice: Option<String>,
    main: HWND,
    opened_at: Option<Instant>,
    text: Option<LiveText>,
    reader: Reader,
    ocr_words: Vec<Word>,
    ocr_pending: usize,
    ocr_seq: u64,
    keep_lines: bool,
    read_on_freeze: bool,
    text_bar: Vec<(BarBtn, Rect)>,
    text_pressed: Option<BarBtn>,
    /// Record: the chosen area, waiting for Start. The recorder is already setting up.
    rec_area: Option<Rect>,
    rec_chip: Option<Rect>,
    rec_fps: u32,
}

thread_local! {
    static OV: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Run `f` on the overlay unless it's already borrowed (re-entrant window messages).
pub fn with<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OV.with(|c| c.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

pub fn install(o: Overlay) {
    OV.with(|c| *c.borrow_mut() = Some(o));
}

pub fn uninstall() {
    let o = OV.with(|c| c.borrow_mut().take());
    if let Some(mut o) = o {
        o.close();
        for w in o.wins.drain(..) {
            unsafe {
                let _ = DestroyWindow(w.hwnd);
            }
        }
        drop(o);
    }
}

const CLASS: windows::core::PCWSTR = w!("FastSnipOverlay");

pub fn register_class() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

fn lparam_xy(l: LPARAM) -> (i32, i32) {
    (
        (l.0 & 0xFFFF) as i16 as i32,
        ((l.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = with(|o| o.on_message(hwnd, msg, wp, lp)).flatten();
    match handled {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl Overlay {
    pub fn new(main: HWND, cfg: &Config) -> windows::core::Result<Self> {
        let gfx = Gfx::new()?;
        let dash = gfx.dash_style();
        let dup = Dup::new(&gfx.d3d).ok();
        if dup.is_none() {
            log("desktop duplication unavailable, using GDI grab");
        }
        let mut o = Self {
            gfx,
            dup,
            wins: Vec::new(),
            visible: false,
            frame: None,
            snapshot: Vec::new(),
            intent: Intent::Snip,
            shape: Shape::Rect,
            mouse: (0, 0),
            drag: None,
            hover: None,
            pressed: None,
            tb_mon: 0,
            full_mon: None,
            element: None,
            elem_seq: 0,
            finder: Finder::start(main),
            mic: true,
            audio: true,
            palette: theme::palette(&cfg.look.accent, &cfg.look.theme),
            magnifier: cfg.look.magnifier,
            animations: cfg.look.animations,
            anim_speed: cfg.look.animation_speed.clamp(0.5, 3.0),
            anim_t0: None,
            shots_dir: output::screenshots_dir(&cfg.saving.screenshots_dir),
            dash,
            notice: None,
            main,
            opened_at: None,
            text: None,
            reader: Reader::start(main),
            ocr_words: Vec::new(),
            ocr_pending: 0,
            ocr_seq: 0,
            keep_lines: cfg.text.keep_line_breaks,
            read_on_freeze: cfg.text.read_on_freeze,
            text_bar: Vec::new(),
            text_pressed: None,
            rec_area: None,
            rec_chip: None,
            rec_fps: cfg.recording.fps,
        };
        // Pre-create the windows and swap chains, and draw once so fonts and
        // icons are cached: the first open is as fast as every later one.
        o.sync_windows();
        // Warm up the animated path too (layers are set up on first use).
        o.anim_t0 = Some(Instant::now());
        o.render_all();
        o.anim_t0 = None;
        o.render_all();
        Ok(o)
    }

    pub fn apply_config(&mut self, cfg: &Config) {
        self.palette = theme::palette(&cfg.look.accent, &cfg.look.theme);
        self.magnifier = cfg.look.magnifier;
        self.animations = cfg.look.animations;
        self.anim_speed = cfg.look.animation_speed.clamp(0.5, 3.0);
        self.keep_lines = cfg.text.keep_line_breaks;
        self.read_on_freeze = cfg.text.read_on_freeze;
        self.rec_fps = cfg.recording.fps;
        self.shots_dir = output::screenshots_dir(&cfg.saving.screenshots_dir);
    }

    /// Make one overlay window per monitor, matching current monitor rects.
    fn sync_windows(&mut self) {
        let mons = capture::monitors();
        let same = mons.len() == self.wins.len()
            && mons
                .iter()
                .zip(&self.wins)
                .all(|(m, w)| m.rect == w.mon.rect && m.dpi == w.mon.dpi);
        if same {
            return;
        }
        for w in self.wins.drain(..) {
            unsafe {
                let _ = DestroyWindow(w.hwnd);
            }
        }
        let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        for m in mons {
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                    CLASS,
                    &HSTRING::from("FastSnip"),
                    WS_POPUP,
                    m.rect.x,
                    m.rect.y,
                    m.rect.w,
                    m.rect.h,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                )
            };
            let Ok(hwnd) = hwnd else { continue };
            let Ok(surf) = self.gfx.surface(hwnd, m.rect.w as u32, m.rect.h as u32) else {
                continue;
            };
            self.wins.push(OvWin {
                hwnd,
                mon: m,
                surf: Some(surf),
                frozen: None,
            });
        }
    }

    /// Windows allows one desktop duplication per display per process, so
    /// the overlay hands it to the recorder while recording (screenshots in
    /// the meantime use the GDI grab).
    pub fn suspend_dup(&mut self) {
        self.dup = None;
    }

    pub fn resume_dup(&mut self) {
        if self.dup.is_none() && !crate::recui::is_busy() {
            self.dup = Dup::new(&self.gfx.d3d).ok();
        }
    }

    pub fn displays_changed(&mut self) {
        if let Some(d) = &mut self.dup {
            d.reset();
        }
        self.sync_windows();
    }

    fn mon_at(&self, x: i32, y: i32) -> usize {
        self.wins
            .iter()
            .position(|w| w.mon.rect.contains(x, y))
            .unwrap_or(0)
    }

    // ---------------------------------------------------------------- open / close

    pub fn on_action(&mut self, a: Action) {
        match a {
            Action::OpenToolbar => self.open(Intent::Snip, None),
            Action::GrabText => self.open(Intent::Text, Some(Shape::Rect)),
            Action::RecordFullScreen => {
                // The shortcut toggles: stop if recording, else record the display under the mouse.
                if crate::recui::is_busy() {
                    crate::recui::stop();
                } else {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    let mons = capture::monitors();
                    if let Some(m) = mons.iter().find(|m| m.rect.contains(p.x, p.y)).or(mons.first()) {
                        self.suspend_dup();
                        crate::recui::prepare(m.rect, self.mic, self.audio);
                        crate::recui::begin();
                    }
                }
            }
            Action::SnipFullScreen => self.snip_full_screen_now(),
        }
    }

    /// Full screen shot of the display under the mouse, no overlay at all.
    fn snip_full_screen_now(&mut self) {
        let Some(frame) = capture::grab() else { return };
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let mons = capture::monitors();
        let r = mons
            .iter()
            .find(|m| m.rect.contains(p.x, p.y))
            .map(|m| m.rect)
            .unwrap_or(frame.bounds);
        self.frame = Some(frame);
        self.deliver(Target::Area(r));
        self.frame = None;
    }

    pub fn open(&mut self, intent: Intent, shape: Option<Shape>) {
        if self.visible {
            if intent != self.intent {
                self.intent = intent;
                self.render_all();
            }
            return;
        }
        let t0 = Instant::now();
        // Freeze first: GPU copy of every monitor (~1-2 ms), GDI as fallback.
        let shots = self.dup.as_mut().map(|d| d.grab()).unwrap_or_default();
        let t_grab = t0.elapsed();
        self.sync_windows();
        let gpu_ok = !shots.is_empty()
            && self
                .wins
                .iter()
                .all(|w| shots.iter().any(|s| s.rect == w.mon.rect));
        let mut cpu_frame = None;
        if gpu_ok {
            for w in &mut self.wins {
                let shot = shots.iter().find(|s| s.rect == w.mon.rect).unwrap();
                w.frozen = self.gfx.bitmap_from_texture(&shot.tex).ok();
            }
        } else {
            let Some(frame) = capture::grab() else { return };
            let stride = frame.bounds.w as u32 * 4;
            for w in &mut self.wins {
                let ox = (w.mon.rect.x - frame.bounds.x) as usize;
                let oy = (w.mon.rect.y - frame.bounds.y) as usize;
                let off = oy * stride as usize + ox * 4;
                let ptr = unsafe { frame.pixels.as_ptr().add(off) };
                w.frozen = self
                    .gfx
                    .bitmap(ptr, w.mon.rect.w as u32, w.mon.rect.h as u32, stride)
                    .ok();
            }
            cpu_frame = Some(frame);
        }
        self.snapshot = capture::windows(std::process::id());
        let t_snap = t0.elapsed();
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        self.mouse = (p.x, p.y);
        self.tb_mon = self.mon_at(p.x, p.y);
        self.full_mon = None;
        self.intent = intent;
        if let Some(s) = shape {
            self.shape = s;
        }
        self.drag = None;
        self.hover = None;
        self.pressed = None;
        self.element = None;
        self.notice = None;
        self.frame = cpu_frame;
        let t_upload = t0.elapsed();
        self.visible = true;
        self.opened_at = Some(t0);
        self.anim_t0 = self.animations.then(Instant::now);
        self.render_all();
        let t_render = t0.elapsed();
        for w in &self.wins {
            unsafe {
                let _ = SetWindowPos(
                    w.hwnd,
                    Some(HWND_TOPMOST),
                    w.mon.rect.x,
                    w.mon.rect.y,
                    w.mon.rect.w,
                    w.mon.rect.h,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                );
            }
        }
        let t_show = t0.elapsed();
        if let Some(w) = self.wins.get(self.tb_mon) {
            if self.anim_t0.is_some() {
                unsafe {
                    SetTimer(Some(w.hwnd), ANIM_TIMER, 8, None);
                }
            }
            force_foreground(w.hwnd);
        }
        let t_visible = t0.elapsed();
        // CPU copy for saving and the color picker, while the overlay is already up.
        if self.frame.is_none() {
            if let Some(d) = &mut self.dup {
                self.frame = d.readback(&shots, capture::virtual_screen());
            }
        }
        let t_read = t0.elapsed();
        self.text = None;
        self.text_bar.clear();
        self.start_ocr();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        log(&format!(
            "overlay visible in {:.1} ms (grab {:.1} {}, windows {:.1}, setup {:.1}, render {:.1}, show {:.1}, focus {:.1}); pixels ready at {:.1} ms",
            ms(t_visible),
            ms(t_grab),
            if gpu_ok { "gpu" } else { "gdi" },
            ms(t_snap - t_grab),
            ms(t_upload - t_snap),
            ms(t_render - t_upload),
            ms(t_show - t_render),
            ms(t_visible - t_show),
            ms(t_read)
        ));
        if self.shape == Shape::Element {
            self.ask_element();
        }
    }

    pub fn close(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.anim_t0 = None;
        unsafe {
            let _ = ReleaseCapture();
            if let Some(w) = self.wins.get(self.tb_mon) {
                let _ = KillTimer(Some(w.hwnd), ANIM_TIMER);
            }
        }
        for w in &mut self.wins {
            unsafe {
                let _ = ShowWindow(w.hwnd, SW_HIDE);
            }
            w.frozen = None;
        }
        self.frame = None;
        self.snapshot.clear();
        self.drag = None;
        self.text = None;
        self.text_bar.clear();
        self.text_pressed = None;
        if self.rec_area.take().is_some() {
            crate::recui::cancel_prepared();
        }
        self.rec_chip = None;
    }

    // ---------------------------------------------------------------- messages

    fn on_message(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let idx = self.wins.iter().position(|w| w.hwnd == hwnd)?;
        let to_global =
            |o: &Self, (x, y): (i32, i32)| (o.wins[idx].mon.rect.x + x, o.wins[idx].mon.rect.y + y);
        match msg {
            WM_MOUSEMOVE => {
                let g = to_global(self, lparam_xy(lp));
                self.on_move(g);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let g = to_global(self, lparam_xy(lp));
                self.mouse = g;
                if let Some(b) = self.btn_at(g) {
                    self.pressed = Some(b);
                } else if self.rec_chip.map(|r| r.contains(g.0, g.1)).unwrap_or(false) {
                    self.start_recording();
                    return Some(LRESULT(0));
                } else if let Some(tb) = self.text_btn_at(g) {
                    self.text_pressed = Some(tb);
                } else if self.text.as_mut().map(|t| t.press(g)) == Some(true) {
                    unsafe {
                        SetCapture(hwnd);
                    }
                } else {
                    // Outside the text area or chip: start over with a new selection.
                    self.text = None;
                    self.text_bar.clear();
                    if self.rec_area.take().is_some() {
                        crate::recui::cancel_prepared();
                        self.rec_chip = None;
                    }
                    if matches!(self.shape, Shape::Rect | Shape::Free) {
                        self.drag = Some(Drag {
                            start: g,
                            pts: vec![g],
                            mon: self.mon_at(g.0, g.1),
                        });
                        unsafe {
                            SetCapture(hwnd);
                        }
                    }
                }
                self.render_all();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let g = to_global(self, lparam_xy(lp));
                self.on_up(g);
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                self.close();
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                self.on_key(wp.0 as u32);
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                let over_bar = self.btn_at(self.mouse).is_some()
                    || self.in_toolbar(self.mouse)
                    || self.text_btn_at(self.mouse).is_some();
                let in_text = self
                    .text
                    .as_ref()
                    .map(|t| t.region.contains(self.mouse.0, self.mouse.1))
                    .unwrap_or(false);
                let cur = if over_bar {
                    IDC_ARROW
                } else if in_text {
                    IDC_IBEAM
                } else {
                    IDC_CROSS
                };
                unsafe {
                    let c = LoadCursorW(None, cur).unwrap_or_default();
                    SetCursor(Some(c));
                }
                Some(LRESULT(1))
            }
            WM_MOUSEACTIVATE => Some(LRESULT(MA_ACTIVATE as isize)),
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                let mut ps = Default::default();
                unsafe {
                    windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
                    let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
                }
                if self.visible {
                    self.render(idx);
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wp.0 == ANIM_TIMER => {
                if self.visible {
                    self.render_all();
                }
                if self.anim_progress().1 >= 1.0 {
                    self.anim_t0 = None;
                    unsafe {
                        let _ = KillTimer(Some(hwnd), ANIM_TIMER);
                    }
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    pub fn on_element(&mut self, f: element::Found) {
        if self.visible && self.shape == Shape::Element && f.seq == self.elem_seq {
            self.element = Some((f.rect, f.label));
            self.render_all();
        }
    }

    fn ask_element(&mut self) {
        let (x, y) = self.mouse;
        if let Some(win) = self.snapshot.iter().find(|w| w.rect.contains(x, y)) {
            self.elem_seq += 1;
            self.finder.ask(element::Request {
                seq: self.elem_seq,
                window: win.hwnd,
                x,
                y,
            });
        }
    }

    fn on_move(&mut self, g: (i32, i32)) {
        if g == self.mouse {
            return;
        }
        self.mouse = g;
        if let Some(t) = &mut self.text {
            if t.selecting {
                t.drag(g);
                self.render_all();
                return;
            }
        }
        if let Some(d) = &mut self.drag {
            let last = *d.pts.last().unwrap();
            if (last.0 - g.0).abs() + (last.1 - g.1).abs() >= 2 {
                d.pts.push(g);
            }
        } else {
            self.hover = self.btn_at(g);
            if self.shape == Shape::Element && self.hover.is_none() {
                if let Some((r, _)) = &self.element {
                    if !r.contains(g.0, g.1) {
                        self.element = None;
                    }
                }
                self.ask_element();
            }
            if self.shape == Shape::Full {
                self.full_mon = None;
            }
        }
        self.render_all();
    }

    fn on_up(&mut self, g: (i32, i32)) {
        self.mouse = g;
        if let Some(b) = self.text_pressed.take() {
            if self.text_btn_at(g).as_ref() == Some(&b) {
                self.text_action(b);
            }
            if self.visible {
                self.render_all();
            }
            return;
        }
        if let Some(t) = &mut self.text {
            if t.selecting {
                t.release();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.render_all();
                return;
            }
        }
        if let Some(b) = self.pressed.take() {
            if self.btn_at(g) == Some(b) {
                self.activate(b);
            }
            if self.visible {
                self.render_all();
            }
            return;
        }
        if let Some(d) = self.drag.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            // Selections may span displays.
            let clip = capture::virtual_screen();
            match self.shape {
                Shape::Free => {
                    let pts: Vec<(i32, i32)> = d
                        .pts
                        .iter()
                        .map(|&(x, y)| {
                            (
                                x.clamp(clip.x, clip.right()),
                                y.clamp(clip.y, clip.bottom()),
                            )
                        })
                        .collect();
                    let (minx, maxx) = pts
                        .iter()
                        .fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                    let (miny, maxy) = pts
                        .iter()
                        .fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                    if pts.len() >= 3 && maxx - minx >= 3 && maxy - miny >= 3 {
                        self.finish(Target::Shape(pts));
                        return;
                    }
                }
                _ => {
                    let r = Rect::from_points(d.start.0, d.start.1, g.0, g.1);
                    if let Some(r) = r.intersect(&clip) {
                        if r.w >= 3 && r.h >= 3 {
                            self.finish(Target::Area(r));
                            return;
                        }
                    }
                }
            }
            self.render_all();
            return;
        }
        if let Some(r) = self.click_target() {
            self.finish(Target::Area(r));
        }
    }

    /// Area a click takes in window, element and full screen modes.
    fn click_target(&self) -> Option<Rect> {
        let (x, y) = self.mouse;
        match self.shape {
            Shape::Window => self.hovered_window().map(|w| w.rect),
            Shape::Element => self
                .element
                .as_ref()
                .map(|e| e.0)
                .or_else(|| self.hovered_window().map(|w| w.rect)),
            Shape::Full => {
                let _ = (x, y);
                Some(self.full_target().0)
            }
            _ => None,
        }
        .and_then(|r| r.intersect(&capture::virtual_screen()))
    }

    /// Full screen target: the display under the mouse (or picked with Tab), or all displays.
    fn full_target(&self) -> (Rect, String) {
        let i = self.full_mon.unwrap_or_else(|| self.mon_at(self.mouse.0, self.mouse.1));
        if i >= self.wins.len() {
            let mut u = self.wins[0].mon.rect;
            for w in &self.wins[1..] {
                let r = w.mon.rect;
                let (x, y) = (u.x.min(r.x), u.y.min(r.y));
                u = Rect { x, y, w: u.right().max(r.right()) - x, h: u.bottom().max(r.bottom()) - y };
            }
            return (u, format!("All {} displays · {} × {} · Enter", self.wins.len(), u.w, u.h));
        }
        let r = self.wins[i].mon.rect;
        let tab = if self.wins.len() > 1 { " · Tab for next" } else { "" };
        (r, format!("Display {} · {} × {} · Enter{tab}", i + 1, r.w, r.h))
    }

    fn hovered_window(&self) -> Option<&WinInfo> {
        let (x, y) = self.mouse;
        self.snapshot.iter().find(|w| w.rect.contains(x, y))
    }

    fn on_key(&mut self, vk: u32) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        if ctrl {
            if let Some(t) = &mut self.text {
                if vk == b'A' as u32 {
                    t.select_all();
                } else if vk == b'C' as u32 {
                    self.copy_text();
                }
                self.render_all();
            }
            return;
        }
        match vk {
            v if v == VK_ESCAPE.0 as u32 => return self.close(),
            v if v == b'S' as u32 => self.set_intent(Intent::Snip),
            v if v == b'R' as u32 => self.set_intent(Intent::Record),
            v if v == b'T' as u32 => self.set_intent(Intent::Text),
            v if (b'1' as u32..=b'5' as u32).contains(&v) => {
                self.set_shape(SHAPES[(v - b'1' as u32) as usize].0)
            }
            v if v == b'M' as u32 && self.intent == Intent::Record => self.mic = !self.mic,
            v if v == b'A' as u32 && self.intent == Intent::Record => self.audio = !self.audio,
            v if v == b'O' as u32 => return self.activate(Btn::OpenApp),
            v if v == b'C' as u32 => {
                if let Some(px) = self
                    .frame
                    .as_ref()
                    .and_then(|f| f.pixel(self.mouse.0, self.mouse.1))
                {
                    let hex = format!("#{:02X}{:02X}{:02X}", px[0], px[1], px[2]);
                    output::clipboard_set_text(self.main, &hex);
                    self.notice = Some(format!("Copied {hex}"));
                }
            }
            v if v == VK_TAB.0 as u32 && self.shape == Shape::Full => {
                // Each display, then "All displays" when there's more than one.
                let n = self.wins.len().max(1) + usize::from(self.wins.len() > 1);
                let cur = self
                    .full_mon
                    .unwrap_or_else(|| self.mon_at(self.mouse.0, self.mouse.1));
                self.full_mon = Some(if shift {
                    (cur + n - 1) % n
                } else {
                    (cur + 1) % n
                });
            }
            v if v == VK_RETURN.0 as u32 && self.rec_area.is_some() => {
                return self.start_recording();
            }
            v if v == VK_RETURN.0 as u32 => {
                let r = match self.shape {
                    Shape::Window | Shape::Element => self.click_target(),
                    _ => Some(self.full_target().0),
                };
                if let Some(r) = r {
                    return self.finish(Target::Area(r));
                }
            }
            v if (VK_LEFT.0 as u32..=VK_DOWN.0 as u32).contains(&v) => {
                let step = if shift { 10 } else { 1 };
                let (dx, dy) = match v {
                    x if x == VK_LEFT.0 as u32 => (-step, 0),
                    x if x == VK_RIGHT.0 as u32 => (step, 0),
                    x if x == VK_UP.0 as u32 => (0, -step),
                    _ => (0, step),
                };
                let p = (self.mouse.0 + dx, self.mouse.1 + dy);
                unsafe {
                    let _ = SetCursorPos(p.0, p.1);
                }
                self.on_move(p);
                return;
            }
            _ => return,
        }
        if self.visible {
            self.render_all();
        }
    }

    fn set_intent(&mut self, i: Intent) {
        if i != Intent::Text {
            self.text = None;
            self.text_bar.clear();
        }
        if i != Intent::Record && self.rec_area.take().is_some() {
            crate::recui::cancel_prepared();
            self.rec_chip = None;
        }
        self.intent = i;
    }

    fn set_shape(&mut self, s: Shape) {
        self.shape = s;
        self.element = None;
        self.full_mon = None;
        if s == Shape::Element {
            self.ask_element();
        }
    }

    fn activate(&mut self, b: Btn) {
        match b {
            Btn::Intent(i) => self.set_intent(i),
            Btn::Shape(s) => self.set_shape(s),
            Btn::Mic => self.mic = !self.mic,
            Btn::Audio => self.audio = !self.audio,
            Btn::Close => self.close(),
            Btn::OpenApp => {
                self.close();
                open_app();
            }
        }
    }

    // ---------------------------------------------------------------- capture

    fn finish(&mut self, t: Target) {
        match self.intent {
            Intent::Snip => {
                self.deliver(t);
                self.close();
            }
            Intent::Record => {
                if crate::recui::is_busy() {
                    self.notice = Some("Already recording. Stop it from the pill first".into());
                    self.render_all();
                    return;
                }
                let r = match t {
                    Target::Area(r) => r,
                    Target::Shape(pts) => {
                        let (minx, maxx) = pts.iter().fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                        let (miny, maxy) = pts.iter().fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                        Rect { x: minx, y: miny, w: maxx - minx, h: maxy - miny }
                    }
                };
                // Start the slow encoder setup now; Start + countdown hide it.
                self.suspend_dup();
                crate::recui::prepare(r, self.mic, self.audio);
                self.rec_area = Some(r);
                self.render_all();
            }
            Intent::Text => {
                let r = match t {
                    Target::Area(r) => r,
                    Target::Shape(pts) => {
                        let (minx, maxx) = pts.iter().fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                        let (miny, maxy) = pts.iter().fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                        Rect { x: minx, y: miny, w: maxx - minx, h: maxy - miny }
                    }
                };
                self.start_text(r);
                self.render_all();
            }
        }
    }

    fn start_recording(&mut self) {
        if self.rec_area.take().is_none() {
            return;
        }
        self.rec_chip = None;
        crate::recui::begin();
        self.close();
    }

    fn draw_rec_pending(&mut self, idx: usize) {
        let p = self.palette;
        let mon = self.wins[idx].mon.clone();
        let (mw, mh) = (mon.rect.w as f32, mon.rect.h as f32);
        let s = mon.scale();
        let Some(area) = self.rec_area else { return };
        let loc = |r: &Rect| rf((r.x - mon.rect.x) as f32, (r.y - mon.rect.y) as f32, r.w as f32, r.h as f32);
        match area.intersect(&mon.rect) {
            Some(ri) => {
                let lr = loc(&ri);
                self.dim_outside(lr, mw, mh);
                let inset = if ri == mon.rect { 1.5 * s } else { 0.0 };
                self.gfx.stroke(
                    rf(lr.left + inset, lr.top + inset, lr.right - lr.left - inset * 2.0, lr.bottom - lr.top - inset * 2.0),
                    p.rec,
                    2.0 * s,
                    self.dash.as_ref(),
                );
            }
            None => {
                self.gfx.fill(rf(0.0, 0.0, mw, mh), p.dim);
                return;
            }
        }
        if self.mon_at(area.x + area.w / 2, area.y + area.h / 2) != idx {
            return;
        }
        let lr = loc(&area);
        let f = self.gfx.fonts(s).label.clone();
        let k = self.gfx.fonts(s).key.clone();
        let start = "Start recording";
        let info = format!("{} × {} · {} fps", area.w & !1, area.h & !1, self.rec_fps);
        let (sw, th) = self.gfx.text_size(start, &f);
        let (kw, _) = self.gfx.text_size("Enter", &k);
        let (iw, _) = self.gfx.text_size(&info, &f);
        let bh = 32.0 * s;
        let start_w = 12.0 * s + 16.0 * s + 7.0 * s + sw + 8.0 * s + kw + 10.0 * s + 12.0 * s;
        let total = 3.0 * s + start_w + 2.0 * s + iw + 24.0 * s + 3.0 * s;
        let x = ((lr.left + lr.right) / 2.0 - total / 2.0).clamp(4.0, (mw - total - 4.0).max(4.0));
        let mut y = lr.bottom + 10.0 * s;
        if y + bh + 6.0 * s > mh - 4.0 {
            y = (lr.bottom - bh - 22.0 * s).max(4.0);
        }
        let outer = rf(x, y, total, bh + 6.0 * s);
        self.gfx.fill_round(outer, 10.0 * s, p.bar);
        self.gfx.stroke_round(outer, 10.0 * s, p.bar_line, 1.0);
        let br = rf(x + 3.0 * s, y + 3.0 * s, start_w, bh);
        self.gfx.fill_round(br, 7.0 * s, p.rec);
        let white = theme::rgb(0xffffff);
        let cy = br.top + bh / 2.0;
        self.gfx.icon("circle-dot", br.left + 12.0 * s, cy - 8.0 * s, 16.0 * s, white);
        self.gfx.text(start, &f, br.left + 35.0 * s, cy - th / 2.0, white);
        let kr = rf(br.left + 35.0 * s + sw + 8.0 * s, cy - 8.0 * s, kw + 10.0 * s, 16.0 * s);
        self.gfx.stroke_round(kr, 4.0 * s, theme::rgba(0xffffff, 0.6), 1.0);
        self.gfx.text_center("Enter", &k, kr, white);
        self.gfx.text(&info, &f, br.right + 14.0 * s, cy - th / 2.0, p.fg);
        self.rec_chip = Some(Rect { x: mon.rect.x + br.left as i32, y: mon.rect.y + br.top as i32, w: start_w as i32, h: bh as i32 });
    }

    // ---------------------------------------------------------------- text mode

    /// Read every monitor in the background as soon as the screen freezes.
    fn start_ocr(&mut self) {
        self.ocr_seq += 1;
        self.ocr_words.clear();
        self.ocr_pending = 0;
        if !(self.read_on_freeze || self.intent == Intent::Text) {
            return;
        }
        let Some(frame) = &self.frame else { return };
        let mut order: Vec<usize> = (0..self.wins.len()).collect();
        order.sort_by_key(|&i| i != self.tb_mon);
        for i in order {
            let r = self.wins[i].mon.rect;
            if let Some((r, bgra)) = frame.crop(&r) {
                self.reader.read(ocr::Job {
                    seq: self.ocr_seq,
                    kind: ocr::Kind::Monitor,
                    origin: (r.x, r.y),
                    w: r.w as u32,
                    h: r.h as u32,
                    bgra,
                    upscale: false,
                });
                self.ocr_pending += 1;
            }
        }
    }

    fn start_text(&mut self, r: Rect) {
        let words = ocr::words_in(&self.ocr_words, &r);
        let mut queued = false;
        // Re-read small areas at 2x: much better on tiny UI text, still fast.
        let small = (r.w as i64 * r.h as i64) <= 600_000;
        if small || self.ocr_pending == 0 && self.ocr_words.is_empty() {
            if let Some((cr, bgra)) = self.frame.as_ref().and_then(|f| f.crop(&r)) {
                self.reader.read(ocr::Job {
                    seq: self.ocr_seq,
                    kind: ocr::Kind::Region,
                    origin: (cr.x, cr.y),
                    w: cr.w as u32,
                    h: cr.h as u32,
                    bgra,
                    upscale: small,
                });
                queued = true;
            }
        }
        let reading = words.is_empty() && (queued || self.ocr_pending > 0);
        self.text = Some(LiveText::new(r, words, reading));
        self.text_bar.clear();
    }

    pub fn on_ocr(&mut self, d: ocr::Done) {
        if d.seq != self.ocr_seq || !self.visible {
            return;
        }
        log(&format!("ocr {:?}: {} words in {:.0} ms", d.kind, d.words.len(), d.ms));
        match d.kind {
            ocr::Kind::Monitor => {
                self.ocr_pending = self.ocr_pending.saturating_sub(1);
                let base = self.ocr_words.iter().map(|w| w.line + 1).max().unwrap_or(0);
                self.ocr_words.extend(d.words.into_iter().map(|mut w| {
                    w.line += base;
                    w
                }));
                let pending = self.ocr_pending;
                if let Some(t) = &mut self.text {
                    if !t.refined && t.sel.is_none() {
                        let words = ocr::words_in(&self.ocr_words, &t.region);
                        if !words.is_empty() {
                            t.set_words(words);
                        }
                    }
                    t.reading = t.words.is_empty() && (pending > 0 || !t.refined);
                }
            }
            ocr::Kind::Region => {
                if let Some(t) = &mut self.text {
                    if t.region == d.area || (t.region.w == d.area.w && t.region.h == d.area.h) {
                        let mut words = ocr::words_in(&d.words, &t.region);
                        ocr::sort_reading(&mut words);
                        if t.sel.is_none() || t.words.is_empty() {
                            t.set_words(words);
                        }
                        t.refined = true;
                        t.reading = false;
                    }
                }
            }
        }
        self.render_all();
    }

    fn text_btn_at(&self, g: (i32, i32)) -> Option<BarBtn> {
        self.text_bar.iter().find(|(_, r)| r.contains(g.0, g.1)).map(|(b, _)| b.clone())
    }

    fn copy_text(&mut self) {
        let Some(t) = &self.text else { return };
        let text = t.text(self.keep_lines);
        if text.is_empty() {
            return;
        }
        let n = t.selected().len();
        output::clipboard_set_text(self.main, &text);
        self.notice = Some(format!("Copied {n} {} · Esc to close", if n == 1 { "word" } else { "words" }));
    }

    fn text_action(&mut self, b: BarBtn) {
        match b {
            BarBtn::Copy => self.copy_text(),
            BarBtn::All => {
                if let Some(t) = &mut self.text {
                    t.select_all();
                }
            }
            BarBtn::Link(url) => {
                self.close();
                shell_open(&url);
            }
            BarBtn::Search => {
                let q = self.text.as_ref().map(|t| t.text(false)).unwrap_or_default();
                if !q.is_empty() {
                    self.close();
                    shell_open(&search_url(&q));
                }
            }
        }
    }

    fn draw_live_text(&mut self, idx: usize) {
        let p = self.palette;
        let mon = self.wins[idx].mon.clone();
        let (mw, mh) = (mon.rect.w as f32, mon.rect.h as f32);
        let s = mon.scale();
        let Some(t) = &self.text else { return };
        let region = t.region;
        let loc = |r: &Rect| rf((r.x - mon.rect.x) as f32, (r.y - mon.rect.y) as f32, r.w as f32, r.h as f32);
        match region.intersect(&mon.rect) {
            Some(ri) => {
                let lr = loc(&ri);
                self.dim_outside(lr, mw, mh);
                self.gfx.stroke(lr, theme::rgb(0xffffff), 1.5 * s, None);
            }
            None => self.gfx.fill(rf(0.0, 0.0, mw, mh), p.dim),
        }
        let sel = t.sel;
        for (i, w) in t.words.iter().enumerate() {
            if !mon.rect.contains(w.rect.x, w.rect.y) {
                continue;
            }
            let r = loc(&w.rect);
            let on = matches!(sel, Some((a, b)) if i >= a && i <= b);
            let pad = 2.0 * s;
            self.gfx.fill_round(
                rf(r.left - pad, r.top - pad, r.right - r.left + pad * 2.0, r.bottom - r.top + pad * 2.0),
                3.0 * s,
                if on { p.ocr_sel } else { p.ocr },
            );
        }
        let home = self.mon_at(region.x + region.w / 2, region.y + region.h / 2);
        if idx != home {
            return;
        }
        let hint = if t.reading {
            Some("Reading text…")
        } else if t.words.is_empty() {
            Some("No text found. Try a bigger area")
        } else if sel.is_none() {
            Some("Drag across words · double-click a word · Ctrl+A for all")
        } else {
            None
        };
        let lr = loc(&region);
        if let Some(h) = hint {
            let f = self.gfx.fonts(s).label.clone();
            let (tw, th) = self.gfx.text_size(h, &f);
            let (bw, bh) = (tw + 24.0 * s, th + 10.0 * s);
            let x = ((lr.left + lr.right) / 2.0 - bw / 2.0).clamp(4.0, (mw - bw - 4.0).max(4.0));
            let mut y = lr.top - bh - 8.0 * s;
            if y < 4.0 {
                y = lr.top + 8.0 * s;
            }
            let r = rf(x, y, bw, bh);
            self.gfx.fill_round(r, bh / 2.0, p.bar);
            self.gfx.stroke_round(r, bh / 2.0, p.bar_line, 1.0);
            self.gfx.text(h, &f, x + 12.0 * s, y + 5.0 * s, p.fg);
        }
        // Copy bar next to the selection.
        let mut bar_rects = Vec::new();
        if let Some((a, b)) = sel {
            let t = self.text.as_ref().unwrap();
            let (first, last) = (t.words[a].rect, t.words[b].rect);
            let buttons = t.bar_buttons(self.keep_lines);
            let f = self.gfx.fonts(s).label.clone();
            let (bh, pad, icon) = (30.0 * s, 10.0 * s, 15.0 * s);
            let widths: Vec<f32> = buttons
                .iter()
                .map(|(_, l, _)| {
                    let lw = if l.is_empty() { 0.0 } else { self.gfx.text_size(l, &f).0 + 6.0 * s };
                    pad * 2.0 + icon + lw
                })
                .collect();
            let total: f32 = widths.iter().sum::<f32>() + 2.0 * s * (widths.len() as f32 - 1.0) + 6.0 * s;
            let lf = loc(&first);
            let ll = loc(&last);
            let mut x = (ll.left - 40.0 * s).clamp(4.0, (mw - total - 4.0).max(4.0));
            let mut y = ll.bottom + 10.0 * s;
            if y + bh + 6.0 * s > mh - 4.0 {
                y = lf.top - bh - 16.0 * s;
                x = (lf.left - 40.0 * s).clamp(4.0, (mw - total - 4.0).max(4.0));
            }
            let outer = rf(x, y, total, bh + 6.0 * s);
            self.gfx.fill_round(outer, 10.0 * s, p.bar);
            self.gfx.stroke_round(outer, 10.0 * s, p.bar_line, 1.0);
            let mut bx = x + 3.0 * s;
            for ((btn, label, ic), w) in buttons.into_iter().zip(widths) {
                let r = rf(bx, y + 3.0 * s, w, bh);
                let g = Rect { x: mon.rect.x + r.left as i32, y: mon.rect.y + r.top as i32, w: w as i32, h: bh as i32 };
                let hovered = g.contains(self.mouse.0, self.mouse.1);
                let primary = btn == BarBtn::Copy;
                if primary {
                    self.gfx.fill_round(r, 7.0 * s, p.accent);
                } else if hovered {
                    self.gfx.fill_round(r, 7.0 * s, p.hover);
                }
                let c = if primary { p.on_accent } else if matches!(btn, BarBtn::Link(_)) { p.accent } else { p.fg };
                let cy = r.top + bh / 2.0;
                self.gfx.icon(ic, r.left + pad, cy - icon / 2.0, icon, c);
                if !label.is_empty() {
                    let (_, lh) = self.gfx.text_size(&label, &f);
                    self.gfx.text(&label, &f, r.left + pad + icon + 6.0 * s, cy - lh / 2.0, c);
                }
                bar_rects.push((btn, g));
                bx += w + 2.0 * s;
            }
        }
        self.text_bar = bar_rects;
    }

    /// Cut the image from the frozen frame, put it on the clipboard, save it.
    fn deliver(&mut self, t: Target) {
        let area = match &t {
            Target::Area(r) => *r,
            Target::Shape(pts) => {
                let (minx, maxx) = pts.iter().fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                let (miny, maxy) = pts.iter().fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                Rect { x: minx, y: miny, w: maxx - minx, h: maxy - miny }
            }
        };
        let Some(frame) = &self.frame else { return };
        let img = match t {
            Target::Area(r) => frame.crop(&r).map(|(r, px)| Image {
                w: r.w as u32,
                h: r.h as u32,
                bgra: px,
            }),
            Target::Shape(pts) => {
                let (minx, maxx) = pts
                    .iter()
                    .fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                let (miny, maxy) = pts
                    .iter()
                    .fold((i32::MAX, i32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                let bbox = Rect {
                    x: minx,
                    y: miny,
                    w: maxx - minx,
                    h: maxy - miny,
                };
                frame.crop(&bbox).map(|(r, mut px)| {
                    mask_polygon(&mut px, &r, &pts);
                    Image {
                        w: r.w as u32,
                        h: r.h as u32,
                        bgra: px,
                    }
                })
            }
        };
        let Some(img) = img else { return };
        let img = std::sync::Arc::new(img);
        let t0 = Instant::now();
        output::clipboard_set_image(self.main, &img);
        log(&format!(
            "clipboard in {:.1} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        ));
        // Text for gallery search: reuse the freeze-time read when it's done.
        let index = if !self.read_on_freeze {
            output::TextIndex::Skip
        } else if self.ocr_pending == 0 && !self.ocr_words.is_empty() {
            let mut words = ocr::words_in(&self.ocr_words, &area);
            ocr::sort_reading(&mut words);
            output::TextIndex::Known(ocr::join(&words, true))
        } else {
            output::TextIndex::Read
        };
        output::save_async(img, self.shots_dir.clone(), self.main, index);
    }

    // ---------------------------------------------------------------- toolbar layout

    fn scale(&self) -> f32 {
        self.wins
            .get(self.tb_mon)
            .map(|w| w.mon.scale())
            .unwrap_or(1.0)
    }

    /// Toolbar rect and button rects, in local coords of the toolbar monitor.
    fn layout(&self) -> (D2D_RECT_F, D2D_RECT_F, Vec<(Btn, D2D_RECT_F)>) {
        let s = self.scale();
        let mon_w = self
            .wins
            .get(self.tb_mon)
            .map(|w| w.mon.rect.w as f32)
            .unwrap_or(1920.0);
        let (pad, seg_item, seg_h, btn_w, btn_h, key_h, gap, sep) =
            (6.0, 96.0, 32.0, 36.0, 34.0, 13.0, 2.0, 11.0);
        // row 2 items: (Some(btn), width) or (None, sep)
        let mut row: Vec<(Option<Btn>, f32)> = SHAPES
            .iter()
            .map(|(sh, _, _)| (Some(Btn::Shape(*sh)), btn_w))
            .collect();
        if self.intent == Intent::Record {
            row.push((None, sep));
            row.push((Some(Btn::Mic), btn_w));
            row.push((Some(Btn::Audio), btn_w));
        }
        row.push((None, sep));
        row.push((Some(Btn::OpenApp), btn_w));
        row.push((Some(Btn::Close), btn_w));
        let row_w: f32 = row.iter().map(|r| r.1).sum::<f32>() + gap * (row.len() as f32 - 1.0);
        let seg_w = seg_item * 3.0 + 6.0;
        let inner = row_w.max(seg_w);
        let bar_w = inner + pad * 2.0;
        let bar_h = pad + seg_h + pad + btn_h + key_h + pad;
        let bx = ((mon_w / s) - bar_w) / 2.0;
        let by = 10.0;
        let bar = rf(bx * s, by * s, bar_w * s, bar_h * s);
        let seg = rf(
            (bx + pad + (inner - seg_w) / 2.0) * s,
            (by + pad) * s,
            seg_w * s,
            seg_h * s,
        );
        let mut out = Vec::new();
        for (i, it) in [Intent::Snip, Intent::Record, Intent::Text]
            .iter()
            .enumerate()
        {
            let x = (bx + pad + (inner - seg_w) / 2.0 + 3.0 + i as f32 * seg_item) * s;
            out.push((
                Btn::Intent(*it),
                rf(x, (by + pad + 3.0) * s, seg_item * s, (seg_h - 6.0) * s),
            ));
        }
        let mut x = bx + pad + (inner - row_w) / 2.0;
        let y = by + pad + seg_h + pad;
        for (b, wd) in &row {
            if let Some(b) = b {
                out.push((*b, rf(x * s, y * s, wd * s, (btn_h + key_h) * s)));
            }
            x += wd + gap;
        }
        (bar, seg, out)
    }

    fn local(&self, g: (i32, i32)) -> Option<(f32, f32)> {
        let w = self.wins.get(self.tb_mon)?;
        Some(((g.0 - w.mon.rect.x) as f32, (g.1 - w.mon.rect.y) as f32))
    }

    fn in_toolbar(&self, g: (i32, i32)) -> bool {
        let Some((x, y)) = self.local(g) else {
            return false;
        };
        if !self.wins[self.tb_mon].mon.rect.contains(g.0, g.1) {
            return false;
        }
        let (bar, _, _) = self.layout();
        x >= bar.left && x <= bar.right && y >= bar.top && y <= bar.bottom
    }

    fn btn_at(&self, g: (i32, i32)) -> Option<Btn> {
        if !self.in_toolbar(g) {
            return None;
        }
        let (x, y) = self.local(g)?;
        let (_, _, btns) = self.layout();
        btns.iter()
            .find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(b, _)| *b)
    }

    // ---------------------------------------------------------------- drawing

    fn render_all(&mut self) {
        for i in 0..self.wins.len() {
            self.render(i);
        }
    }

    fn render(&mut self, idx: usize) {
        let Some(mut surf) = self.wins[idx].surf.take() else {
            return;
        };
        if self.gfx.begin(&mut surf).is_ok() {
            self.draw(idx);
            let _ = self.gfx.end(&surf);
        }
        self.wins[idx].surf = Some(surf);
    }

    fn draw(&mut self, idx: usize) {
        let saved = self.palette;
        let (dim_t, _) = self.anim_progress();
        self.palette.dim.a *= ease_out_cubic(dim_t);
        self.draw_inner(idx);
        self.palette = saved;
    }

    fn draw_inner(&mut self, idx: usize) {
        let p = self.palette;
        let mon = self.wins[idx].mon.clone();
        let (mw, mh) = (mon.rect.w as f32, mon.rect.h as f32);
        let s = mon.scale();
        let to_local = |r: &Rect| {
            rf(
                (r.x - mon.rect.x) as f32,
                (r.y - mon.rect.y) as f32,
                r.w as f32,
                r.h as f32,
            )
        };
        if let Some(bmp) = &self.wins[idx].frozen {
            unsafe {
                self.gfx.dc.DrawBitmap(
                    bmp,
                    Some(&rf(0.0, 0.0, mw, mh)),
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                );
            }
        }
        if self.text.is_some() {
            self.draw_live_text(idx);
            self.draw_tail(idx, false);
            return;
        }
        if self.rec_area.is_some() {
            self.draw_rec_pending(idx);
            self.draw_tail(idx, false);
            return;
        }
        let edge = if self.intent == Intent::Record {
            p.rec
        } else {
            theme::rgb(0xffffff)
        };
        let hl = if self.intent == Intent::Record {
            p.rec
        } else {
            p.accent
        };

        // Selection / highlight for this monitor.
        let mut badge: Option<(String, D2D_RECT_F)> = None;
        if let Some(d) = &self.drag {
            if self.shape == Shape::Free {
                let pts: Vec<(f32, f32)> = d
                    .pts
                    .iter()
                    .chain(std::iter::once(&self.mouse))
                    .map(|&(x, y)| ((x - mon.rect.x) as f32, (y - mon.rect.y) as f32))
                    .collect();
                self.gfx.fill(rf(0.0, 0.0, mw, mh), p.dim);
                if let (Some(geo), Some(bmp)) =
                    (self.gfx.polygon(&pts), self.wins[idx].frozen.clone())
                {
                    unsafe {
                        let params = D2D1_LAYER_PARAMETERS1 {
                            contentBounds: rf(-1e6, -1e6, 2e6, 2e6),
                            geometricMask: std::mem::ManuallyDrop::new(Some(geo.cast().unwrap())),
                            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                            maskTransform: Matrix3x2::identity(),
                            opacity: 1.0,
                            opacityBrush: std::mem::ManuallyDrop::new(None),
                            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                        };
                        self.gfx.dc.PushLayer(&params, None);
                        self.gfx.dc.DrawBitmap(
                            &bmp,
                            Some(&rf(0.0, 0.0, mw, mh)),
                            1.0,
                            D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                            None,
                            None,
                        );
                        self.gfx.dc.PopLayer();
                        self.gfx.dc.DrawGeometry(
                            &geo,
                            self.gfx.color(edge),
                            1.5 * s,
                            self.dash.as_ref(),
                        );
                    }
                }
            } else {
                let r = Rect::from_points(d.start.0, d.start.1, self.mouse.0, self.mouse.1);
                // Selections may span displays.
            let clip = capture::virtual_screen();
                match r.intersect(&clip).and_then(|r| r.intersect(&mon.rect)) {
                    Some(r) => {
                        let lr = to_local(&r);
                        self.dim_outside(lr, mw, mh);
                        self.gfx.stroke(
                            lr,
                            edge,
                            1.5 * s,
                            if self.intent == Intent::Record {
                                None
                            } else {
                                self.dash.as_ref()
                            },
                        );
                        badge = Some((format!("{} × {}", r.w, r.h), lr));
                    }
                    None => self.gfx.fill(rf(0.0, 0.0, mw, mh), p.dim),
                }
            }
        } else {
            let target = match self.shape {
                Shape::Window | Shape::Element if self.hover.is_none() => {
                    let r = self.click_target();
                    let label = match self.shape {
                        Shape::Element => self
                            .element
                            .as_ref()
                            .map(|e| e.1.clone())
                            .unwrap_or_else(|| "Finding element…".into()),
                        _ => self
                            .hovered_window()
                            .map(|w| w.title.clone())
                            .unwrap_or_default(),
                    };
                    r.map(|r| (r, label))
                }
                Shape::Full => Some(self.full_target()),
                _ => None,
            };
            match target
                .as_ref()
                .and_then(|(r, l)| r.intersect(&mon.rect).map(|ri| (ri, *r, l.clone())))
            {
                Some((ri, full, label)) => {
                    let lr = to_local(&ri);
                    self.dim_outside(lr, mw, mh);
                    let inset = if self.shape == Shape::Full {
                        1.5 * s
                    } else {
                        0.0
                    };
                    let w = if self.shape == Shape::Full {
                        3.0 * s
                    } else {
                        2.0 * s
                    };
                    self.gfx.stroke(
                        rf(
                            lr.left + inset,
                            lr.top + inset,
                            lr.right - lr.left - inset * 2.0,
                            lr.bottom - lr.top - inset * 2.0,
                        ),
                        hl,
                        w,
                        None,
                    );
                    let text = if label.is_empty() {
                        format!("{} × {}", full.w, full.h)
                    } else {
                        format!("{label} · {} × {}", full.w, full.h)
                    };
                    let text = if self.shape == Shape::Full {
                        label
                    } else {
                        text
                    };
                    badge = Some((text, lr));
                }
                None => self.gfx.fill(rf(0.0, 0.0, mw, mh), p.dim),
            }
        }

        if let Some((text, r)) = badge {
            self.draw_badge(&text, r, mw, mh, s, self.shape == Shape::Full);
        }
        self.draw_tail(idx, true);
    }

    /// Toolbar, loupe and notice, drawn last on every monitor.
    fn draw_tail(&mut self, idx: usize, allow_loupe: bool) {
        let p = self.palette;
        let mon = self.wins[idx].mon.clone();
        let (mw, _mh) = (mon.rect.w as f32, mon.rect.h as f32);
        let s = mon.scale();
        if idx == self.tb_mon {
            self.draw_toolbar_animated();
        }
        let over_bar = self.in_toolbar(self.mouse);
        if allow_loupe
            && self.magnifier
            && matches!(self.shape, Shape::Rect | Shape::Free)
            && !over_bar
            && mon.rect.contains(self.mouse.0, self.mouse.1)
        {
            self.draw_loupe(idx);
        }
        if let Some(n) = self.notice.clone() {
            if idx == self.tb_mon {
                let f = self.gfx.fonts(s).label.clone();
                let (tw, th) = self.gfx.text_size(&n, &f);
                let r = rf(
                    (mw - tw) / 2.0 - 12.0 * s,
                    124.0 * s,
                    tw + 24.0 * s,
                    th + 10.0 * s,
                );
                self.gfx.fill_round(r, 8.0 * s, p.fg);
                self.gfx.text(
                    &n,
                    &f,
                    r.left + 12.0 * s,
                    r.top + 5.0 * s,
                    if p.dark {
                        theme::rgb(0x0d1212)
                    } else {
                        theme::rgb(0xffffff)
                    },
                );
            }
        }
    }

    /// (dim fade, toolbar pop) progress, each 0..=1. Always 1 with animations off.
    fn anim_progress(&self) -> (f32, f32) {
        match self.anim_t0 {
            Some(t0) => {
                let ms = t0.elapsed().as_secs_f32() * 1000.0 * self.anim_speed;
                ((ms / DIM_MS).min(1.0), (ms / BAR_MS).min(1.0))
            }
            None => (1.0, 1.0),
        }
    }

    fn dim_outside(&self, r: D2D_RECT_F, mw: f32, mh: f32) {
        let d = self.palette.dim;
        self.gfx.fill(rf(0.0, 0.0, mw, r.top.max(0.0)), d);
        self.gfx
            .fill(rf(0.0, r.bottom, mw, (mh - r.bottom).max(0.0)), d);
        self.gfx
            .fill(rf(0.0, r.top, r.left.max(0.0), r.bottom - r.top), d);
        self.gfx.fill(
            rf(r.right, r.top, (mw - r.right).max(0.0), r.bottom - r.top),
            d,
        );
    }

    fn draw_badge(
        &mut self,
        text: &str,
        sel: D2D_RECT_F,
        mw: f32,
        mh: f32,
        s: f32,
        centered: bool,
    ) {
        let p = self.palette;
        let f = self.gfx.fonts(s).badge.clone();
        let (tw, th) = self.gfx.text_size(text, &f);
        let (bw, bh) = (tw + 16.0 * s, th + 6.0 * s);
        let (mut x, mut y) = if centered {
            ((mw - bw) / 2.0, mh * 0.45)
        } else {
            (sel.left, sel.bottom + 6.0 * s)
        };
        if y + bh > mh - 4.0 {
            y = (sel.top - bh - 6.0 * s).max(4.0);
        }
        x = x.clamp(4.0, (mw - bw - 4.0).max(4.0));
        let r = rf(x, y, bw, bh);
        self.gfx.fill_round(r, 6.0 * s, p.bar);
        self.gfx.stroke_round(r, 6.0 * s, p.bar_line, 1.0);
        self.gfx.text(text, &f, x + 8.0 * s, y + 3.0 * s, p.fg);
    }

    fn draw_toolbar_animated(&mut self) {
        let (_, t) = self.anim_progress();
        if t >= 1.0 {
            return self.draw_toolbar();
        }
        let s = self.scale();
        let (bar, _, _) = self.layout();
        let k = 0.94 + 0.06 * ease_out_back(t);
        let dy = -8.0 * s * (1.0 - ease_out_cubic(t));
        let (cx, cy) = ((bar.left + bar.right) / 2.0, bar.top);
        let m = Matrix3x2::translation(-cx, -cy) * Matrix3x2::scale(k, k) * Matrix3x2::translation(cx, cy + dy);
        unsafe {
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: rf(-1e6, -1e6, 2e6, 2e6),
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: ease_out_cubic(t),
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            self.gfx.dc.SetTransform(&m);
            self.gfx.dc.PushLayer(&params, None);
            self.draw_toolbar();
            self.gfx.dc.PopLayer();
            self.gfx.dc.SetTransform(&Matrix3x2::identity());
        }
    }

    fn draw_toolbar(&mut self) {
        let p = self.palette;
        let s = self.scale();
        let (bar, seg, btns) = self.layout();
        // soft shadow
        for (i, a) in [(3.0, 0.05f32), (2.0, 0.07), (1.0, 0.09)] {
            let e = i * 2.0 * s;
            self.gfx.fill_round(
                rf(
                    bar.left - e / 2.0,
                    bar.top - e / 4.0 + 3.0 * s,
                    bar.right - bar.left + e,
                    bar.bottom - bar.top + e,
                ),
                12.0 * s + e,
                theme::rgba(0, a),
            );
        }
        self.gfx.fill_round(bar, 12.0 * s, p.bar);
        self.gfx.stroke_round(bar, 12.0 * s, p.bar_line, 1.0);
        self.gfx.fill_round(seg, 9.0 * s, p.hover);

        let fonts_label = self.gfx.fonts(s).label.clone();
        let fonts_key = self.gfx.fonts(s).key.clone();
        for (b, r) in &btns {
            let hovered = self.hover == Some(*b);
            match b {
                Btn::Intent(it) => {
                    let on = self.intent == *it;
                    if on {
                        self.gfx.fill_round(*r, 7.0 * s, p.seg_on);
                    } else if hovered {
                        self.gfx.fill_round(*r, 7.0 * s, with_alpha(p.seg_on, 0.5));
                    }
                    let (icon, label, key) = match it {
                        Intent::Snip => ("camera", "Snip", "S"),
                        Intent::Record => ("video", "Record", "R"),
                        Intent::Text => ("scan-text", "Text", "T"),
                    };
                    let c = if !on {
                        p.muted
                    } else {
                        match it {
                            Intent::Snip => p.fg,
                            Intent::Record => p.rec,
                            Intent::Text => p.accent,
                        }
                    };
                    let (lw, lh) = self.gfx.text_size(label, &fonts_label);
                    let (kw, _) = self.gfx.text_size(key, &fonts_key);
                    let kbox = kw + 8.0 * s;
                    let total = 16.0 * s + 7.0 * s + lw + 7.0 * s + kbox;
                    let x0 = r.left + ((r.right - r.left) - total) / 2.0;
                    let cy = (r.top + r.bottom) / 2.0;
                    self.gfx.icon(icon, x0, cy - 8.0 * s, 16.0 * s, c);
                    self.gfx
                        .text(label, &fonts_label, x0 + 23.0 * s, cy - lh / 2.0, c);
                    let kr = rf(x0 + 23.0 * s + lw + 7.0 * s, cy - 8.0 * s, kbox, 16.0 * s);
                    self.gfx.stroke_round(kr, 4.0 * s, p.bar_line, 1.0);
                    self.gfx.text_center(key, &fonts_key, kr, p.muted);
                }
                _ => {
                    let br = rf(r.left, r.top, r.right - r.left, 34.0 * s);
                    let (icon, key, on, off) = match b {
                        Btn::Shape(sh) => {
                            let (_, ic, k) = SHAPES.iter().find(|x| x.0 == *sh).unwrap();
                            (*ic, *k, self.shape == *sh, false)
                        }
                        Btn::Mic => (
                            if self.mic { "mic" } else { "mic-off" },
                            "M",
                            false,
                            !self.mic,
                        ),
                        Btn::Audio => ("volume-2", "A", false, !self.audio),
                        Btn::OpenApp => ("layout-grid", "O", false, false),
                        Btn::Close => ("x", "Esc", false, false),
                        Btn::Intent(_) => unreachable!(),
                    };
                    if on {
                        self.gfx.fill_round(br, 8.0 * s, p.accent_soft);
                    } else if hovered || self.pressed == Some(*b) {
                        self.gfx.fill_round(br, 8.0 * s, p.hover);
                    }
                    let c = if on {
                        p.accent
                    } else if off {
                        p.muted
                    } else {
                        p.fg
                    };
                    let cx = (br.left + br.right) / 2.0;
                    let cy = (br.top + br.bottom) / 2.0;
                    self.gfx.icon(icon, cx - 9.0 * s, cy - 9.0 * s, 18.0 * s, c);
                    self.gfx.text_center(
                        key,
                        &fonts_key,
                        rf(
                            r.left - 6.0 * s,
                            br.bottom,
                            r.right - r.left + 12.0 * s,
                            13.0 * s,
                        ),
                        p.muted,
                    );
                }
            }
        }
        // separators: between the last shape and the next button group, and before open/close.
        let row_btns: Vec<&(Btn, D2D_RECT_F)> = btns
            .iter()
            .filter(|(b, _)| !matches!(b, Btn::Intent(_)))
            .collect();
        for w in row_btns.windows(2) {
            let gap = w[1].1.left - w[0].1.right;
            if gap > 5.0 * s {
                let x = (w[0].1.right + w[1].1.left) / 2.0;
                self.gfx.line(
                    x,
                    w[0].1.top + 4.0 * s,
                    x,
                    w[0].1.top + 30.0 * s,
                    p.bar_line,
                    1.0,
                );
            }
        }
    }

    fn draw_loupe(&mut self, idx: usize) {
        let p = self.palette;
        let mon = self.wins[idx].mon.clone();
        let s = mon.scale();
        let (lx, ly) = (
            (self.mouse.0 - mon.rect.x) as f32,
            (self.mouse.1 - mon.rect.y) as f32,
        );
        let size = 116.0 * s;
        let meta_h = 22.0 * s;
        let mut x = lx + 24.0 * s;
        let mut y = ly + 24.0 * s;
        if x + size > mon.rect.w as f32 - 4.0 {
            x = lx - 24.0 * s - size;
        }
        if y + size + meta_h > mon.rect.h as f32 - 4.0 {
            y = ly - 24.0 * s - size - meta_h;
        }
        let outer = rf(x, y, size, size + meta_h);
        self.gfx.fill_round(outer, 12.0 * s, p.bar);
        if let Some(bmp) = self.wins[idx].frozen.clone() {
            let src = rf(lx - 4.0, ly - 4.0, 9.0, 9.0);
            unsafe {
                let clip = rf(x + 1.0, y + 1.0, size - 2.0, size - 2.0);
                self.gfx
                    .dc
                    .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                self.gfx.dc.DrawBitmap(
                    &bmp,
                    Some(&rf(x, y, size, size)),
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    Some(&src),
                    None,
                );
                self.gfx.dc.PopAxisAlignedClip();
            }
            let cell = size / 9.0;
            self.gfx.stroke(
                rf(x + cell * 4.0, y + cell * 4.0, cell, cell),
                theme::rgb(0xffffff),
                2.0,
                None,
            );
            self.gfx.stroke(
                rf(
                    x + cell * 4.0 - 1.0,
                    y + cell * 4.0 - 1.0,
                    cell + 2.0,
                    cell + 2.0,
                ),
                theme::rgba(0, 0.6),
                1.0,
                None,
            );
        }
        self.gfx.stroke_round(outer, 12.0 * s, p.bar_line, 1.0);
        let f = self.gfx.fonts(s).key.clone();
        let hex = self
            .frame
            .as_ref()
            .and_then(|fr| fr.pixel(self.mouse.0, self.mouse.1))
            .map(|c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]))
            .unwrap_or_default();
        self.gfx.text(
            &format!("{}, {}", self.mouse.0, self.mouse.1),
            &f,
            x + 8.0 * s,
            y + size + 5.0 * s,
            p.fg,
        );
        let (hw, _) = self.gfx.text_size(&hex, &f);
        self.gfx
            .text(&hex, &f, x + size - hw - 8.0 * s, y + size + 5.0 * s, p.fg);
    }
}

/// Zero every pixel outside the polygon (even-odd scanline fill). `px` covers `r`.
pub fn mask_polygon(px: &mut [u8], r: &Rect, pts: &[(i32, i32)]) {
    let n = pts.len();
    let mut xs: Vec<f32> = Vec::with_capacity(16);
    for row in 0..r.h {
        let yc = (r.y + row) as f32 + 0.5;
        xs.clear();
        for i in 0..n {
            let (x0, y0) = (pts[i].0 as f32, pts[i].1 as f32);
            let (x1, y1) = (pts[(i + 1) % n].0 as f32, pts[(i + 1) % n].1 as f32);
            if (y0 <= yc && y1 > yc) || (y1 <= yc && y0 > yc) {
                xs.push(x0 + (yc - y0) / (y1 - y0) * (x1 - x0));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let row_px = &mut px[(row * r.w * 4) as usize..((row + 1) * r.w * 4) as usize];
        let mut inside = vec![false; r.w as usize];
        for pair in xs.chunks_exact(2) {
            let a = ((pair[0] - r.x as f32).ceil().max(0.0)) as usize;
            let b = ((pair[1] - r.x as f32).floor().min(r.w as f32 - 1.0)).max(-1.0);
            if b >= 0.0 {
                for v in inside.iter_mut().take(b as usize + 1).skip(a) {
                    *v = true;
                }
            }
        }
        for (i, inn) in inside.iter().enumerate() {
            if !inn {
                row_px[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
}

/// Bring our window to the front even though the keypress went to another app.
fn force_foreground(hwnd: HWND) {
    unsafe {
        // Fast path: after our own injected input, Windows allows the switch (~1 ms).
        crate::hotkey::send_dummy_key();
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            let _ = SetFocus(Some(hwnd));
            return;
        }
        // Slow path (~30-60 ms): borrow the foreground thread's input state.
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = windows::Win32::System::Threading::GetCurrentThreadId();
        let attached = fg_thread != 0
            && fg_thread != me
            && windows::Win32::System::Threading::AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = windows::Win32::System::Threading::AttachThreadInput(me, fg_thread, false);
        }
    }
}

fn shell_open(target: &str) {
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Start the FastSnip app window (a separate exe next to us).
fn open_app() {
    open_app_with(&[]);
}

/// Start the app window with arguments, e.g. `--edit <file>`.
pub fn open_app_with(args: &[&str]) {
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut candidates = vec![dir.join("FastSnip.App.exe"), dir.join("app").join("FastSnip.App.exe")];
        // Development: core\target\release sits next to app\FastSnip.App\bin.
        if let Some(root) = dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            for cfg in ["Release", "Debug"] {
                candidates.push(
                    root.join("app")
                        .join("FastSnip.App")
                        .join("bin")
                        .join("x64")
                        .join(cfg)
                        .join("net10.0-windows10.0.22621.0")
                        .join("win-x64")
                        .join("FastSnip.App.exe"),
                );
            }
        }
        for app in candidates {
            if app.exists() {
                let _ = std::process::Command::new(app).args(args).spawn();
                return;
            }
        }
    }
    crate::notify::message("The FastSnip window isn't installed");
}

pub fn log(s: &str) {
    let w = HSTRING::from(format!("[fastsnip] {s}\n"));
    unsafe { windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(&w) };
    if std::env::var_os("FASTSNIP_LOG").is_some() {
        eprintln!("[fastsnip] {s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_mask_triangle() {
        let r = Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let mut px = vec![255u8; 10 * 10 * 4];
        mask_polygon(&mut px, &r, &[(0, 0), (10, 0), (0, 10)]);
        let a = |x: usize, y: usize| px[(y * 10 + x) * 4 + 3];
        assert_eq!(a(1, 1), 255); // inside
        assert_eq!(a(9, 9), 0); // outside
    }
}
