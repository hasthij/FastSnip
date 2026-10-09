//! FastSnip core.
//!
//! Two background modes (Settings > Background):
//! - Keep ready in the tray: this process stays loaded with the keyboard hook
//!   and a tray icon, so the overlay opens in one frame (~10 ms).
//! - Start only when needed: a tiny listener (`--listener`, hook only, no
//!   graphics, ~1-2 MB) stays running. A shortcut starts the full core with
//!   the action; that core quits itself after 10 s idle.
//!
//! Command line:
//!   fastsnip.exe            start in the configured mode (does nothing if already running)
//!   fastsnip.exe --open     open the toolbar (starts the core if needed)
//!   fastsnip.exe --text     open the toolbar in Text
//!   fastsnip.exe --full     full screen shot of the display under the mouse
//!   fastsnip.exe --record   start or stop a full screen recording
//!   fastsnip.exe --reload   re-read the settings file (sent by the app window)
//!   fastsnip.exe --quit     stop FastSnip
//!   fastsnip.exe --make-assets <dir>   write the package logos

#![cfg_attr(all(not(debug_assertions), not(feature = "console")), windows_subsystem = "windows")]

mod capture;
mod config;
mod dup;
mod element;
mod gfx;
mod hotkey;
mod icon;
mod livetext;
mod notify;
mod ocr;
mod output;
mod overlay;
mod recorder;
mod recui;
mod theme;
mod tray;

use std::cell::Cell;
use std::time::Instant;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, PAGE_READWRITE,
};
use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Background, Config};
use crate::hotkey::{Action, WM_HOTKEY_ACTION};
use crate::overlay::Overlay;

const MAIN_CLASS: PCWSTR = w!("FastSnipCore");
const LISTENER_CLASS: PCWSTR = w!("FastSnipListener");
const CORE_SHARED: PCWSTR = w!(r"Local\FastSnip.CoreWindow");
const LISTENER_SHARED: PCWSTR = w!(r"Local\FastSnip.ListenerWindow");

/// Posted by a second instance or the app window: wparam = Action, or one of the REMOTE_ codes.
const WM_REMOTE: u32 = WM_APP + 10;
const REMOTE_QUIT: usize = 0;
const REMOTE_CLOSE: usize = 98;
const REMOTE_RELOAD: usize = 99;
const REMOTE_START: usize = 97;

const IDLE_TIMER: usize = 3;
/// On-demand core quits after this long with nothing to do.
const ONESHOT_IDLE_SECS: u64 = 10;
/// Tray core gives memory back to Windows after this long idle.
const TRIM_IDLE_SECS: u64 = 20;

thread_local! {
    static ROLE_TRAY: Cell<bool> = const { Cell::new(true) };
    static IDLE_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    static TRIMMED: Cell<bool> = const { Cell::new(false) };
}

// ------------------------------------------------------------------ finding each other

/// The running core (or listener) writes its window handle here; a second launch reads it.
/// (Searching windows by class can trip over a process that is stuck exiting.)
fn publish(name: PCWSTR, hwnd: HWND) -> Option<HANDLE> {
    unsafe {
        let h = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, 8, name).ok()?;
        let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, 8);
        if view.Value.is_null() {
            return None;
        }
        *(view.Value as *mut i64) = hwnd.0 as i64;
        let _ = UnmapViewOfFile(view);
        Some(h)
    }
}

fn find(name: PCWSTR) -> Option<HWND> {
    unsafe {
        let h = OpenFileMappingW(FILE_MAP_READ.0, false, name).ok()?;
        let view = MapViewOfFile(h, FILE_MAP_READ, 0, 0, 8);
        let hwnd = (!view.Value.is_null()).then(|| HWND(*(view.Value as *const i64) as *mut _));
        if !view.Value.is_null() {
            let _ = UnmapViewOfFile(view);
        }
        let _ = CloseHandle(h);
        let hwnd = hwnd?;
        let mut res = 0usize;
        let ok = SendMessageTimeoutW(hwnd, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 300, Some(&mut res)).0 != 0;
        ok.then_some(hwnd)
    }
}

/// Find a running instance, waiting a little for one that is still starting.
fn find_waiting(name: PCWSTR) -> Option<HWND> {
    for _ in 0..10 {
        if let Some(h) = find(name) {
            return Some(h);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    None
}

fn post(hwnd: HWND, code: usize) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_REMOTE, WPARAM(code), LPARAM(0));
    }
}

fn spawn_self(args: &[&str]) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).args(args).spawn();
    }
}

fn arg_action() -> Option<usize> {
    let a = std::env::args().nth(1)?;
    match a.as_str() {
        "--open" => Some(Action::OpenToolbar as usize),
        "--text" => Some(Action::GrabText as usize),
        "--full" => Some(Action::SnipFullScreen as usize),
        "--record" => Some(Action::RecordFullScreen as usize),
        "--quit" => Some(REMOTE_QUIT),
        "--reload" => Some(REMOTE_RELOAD),
        "--close" => Some(REMOTE_CLOSE),
        _ => None,
    }
}

fn action_arg(a: Action) -> &'static str {
    match a {
        Action::OpenToolbar => "--open",
        Action::GrabText => "--text",
        Action::SnipFullScreen => "--full",
        Action::RecordFullScreen => "--record",
    }
}

fn make_window(class: PCWSTR, proc: WNDPROC) -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A real (hidden) top-level window, not message-only, so it receives
        // broadcasts like TaskbarCreated and WM_SETTINGCHANGE.
        CreateWindowExW(WS_EX_TOOLWINDOW, class, &HSTRING::from("FastSnip"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)
            .expect("main window")
    }
}

fn run_loop() {
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ------------------------------------------------------------------ listener (on-demand mode)

unsafe extern "system" fn listener_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY_ACTION => {
            if let Some(a) = Action::from_usize(wp.0) {
                // Starts the core, or hands the action to one that's still running.
                spawn_self(&[action_arg(a)]);
            }
            LRESULT(0)
        }
        WM_REMOTE => {
            match wp.0 {
                REMOTE_QUIT => PostQuitMessage(0),
                REMOTE_RELOAD => {
                    let cfg = Config::load();
                    if cfg.background == Background::Tray {
                        // Switched to tray mode: the core takes over the shortcuts.
                        spawn_self(&[]);
                        PostQuitMessage(0);
                    } else {
                        hotkey::set_bindings(&cfg.shortcuts);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn run_listener() {
    let _mutex = unsafe { CreateMutexW(None, true, w!(r"Local\FastSnip.Listener")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS && find(LISTENER_SHARED).is_some() {
        return;
    }
    let cfg = Config::load();
    hotkey::set_bindings(&cfg.shortcuts);
    let hwnd = make_window(LISTENER_CLASS, Some(listener_proc));
    let _shared = publish(LISTENER_SHARED, hwnd);
    hotkey::start(hwnd);
    unsafe {
        // Keep the listener as small as it can be.
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
    run_loop();
    hotkey::stop();
    std::process::exit(0);
}

// ------------------------------------------------------------------ core

fn set_role(main: HWND, tray_mode: bool) {
    ROLE_TRAY.with(|r| r.set(tray_mode));
    if tray_mode {
        tray::show(main);
        if let Some(l) = find(LISTENER_SHARED) {
            post(l, REMOTE_QUIT);
        }
    } else {
        tray::hide(main);
    }
}

fn is_idle() -> bool {
    let overlay_open = overlay::with(|o| o.visible).unwrap_or(false);
    !overlay_open && !recui::is_busy() && !recui::is_prepared() && !notify::is_showing() && output::pending() == 0
}

fn on_idle_tick(main: HWND) {
    if !is_idle() {
        IDLE_SINCE.with(|c| c.set(None));
        TRIMMED.with(|t| t.set(false));
        return;
    }
    let since = IDLE_SINCE.with(|c| match c.get() {
        Some(t) => t,
        None => {
            let t = Instant::now();
            c.set(Some(t));
            t
        }
    });
    let idle = since.elapsed().as_secs();
    if ROLE_TRAY.with(|r| r.get()) {
        if idle >= TRIM_IDLE_SECS && !TRIMMED.with(|t| t.replace(true)) {
            unsafe {
                let _ = EmptyWorkingSet(GetCurrentProcess());
            }
        }
    } else if idle >= ONESHOT_IDLE_SECS {
        let _ = main;
        unsafe { PostQuitMessage(0) };
    }
}

fn reload(main: HWND) {
    let cfg = Config::load();
    hotkey::set_bindings(&cfg.shortcuts);
    overlay::with(|o| o.apply_config(&cfg));
    recui::apply_config(&cfg);
    notify::apply_config(&cfg);
    let want_tray = cfg.background == Background::Tray;
    if want_tray != ROLE_TRAY.with(|r| r.get()) {
        if !want_tray {
            // Hand the shortcuts to a listener; this core quits once idle.
            hotkey::stop();
            spawn_self(&["--listener"]);
        } else {
            hotkey::start(main);
        }
        set_role(main, want_tray);
    }
}

fn tray_command(main: HWND, cmd: u32) {
    let act = |a: Action| {
        overlay::with(|o| o.on_action(a));
    };
    match cmd {
        tray::CMD_OPEN => overlay::open_app_with(&[]),
        tray::CMD_SNIP => act(Action::OpenToolbar),
        tray::CMD_FULL => act(Action::SnipFullScreen),
        tray::CMD_RECORD => act(Action::RecordFullScreen),
        tray::CMD_TEXT => act(Action::GrabText),
        tray::CMD_SETTINGS => overlay::open_app_with(&["--settings"]),
        tray::CMD_QUIT => unsafe { PostQuitMessage(0) },
        _ => {}
    }
    let _ = main;
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == tray::taskbar_created_msg() {
        tray::restore(hwnd);
        return LRESULT(0);
    }
    match msg {
        WM_HOTKEY_ACTION | WM_REMOTE => {
            if msg == WM_REMOTE {
                match wp.0 {
                    REMOTE_QUIT => {
                        PostQuitMessage(0);
                        return LRESULT(0);
                    }
                    REMOTE_RELOAD => {
                        reload(hwnd);
                        return LRESULT(0);
                    }
                    REMOTE_CLOSE => {
                        overlay::with(|o| o.close());
                        return LRESULT(0);
                    }
                    REMOTE_START => return LRESULT(0),
                    _ => {}
                }
            }
            if let Some(a) = Action::from_usize(wp.0) {
                overlay::with(|o| o.on_action(a));
            }
            LRESULT(0)
        }
        tray::WM_TRAY => {
            match tray::on_message(hwnd, lp) {
                tray::Ask::Action(a) => {
                    overlay::with(|o| o.on_action(a));
                }
                tray::Ask::Command(c) => tray_command(hwnd, c),
                tray::Ask::None => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == IDLE_TIMER => {
            on_idle_tick(hwnd);
            LRESULT(0)
        }
        output::WM_PNG_READY => {
            let ready = Box::from_raw(lp.0 as *mut output::PngReady);
            output::done_saving();
            output::clipboard_add_png(hwnd, &ready.png);
            if let Some(p) = &ready.path {
                overlay::log(&format!("saved {}", p.display()));
                notify::screenshot_saved(ready.image.clone(), p);
            }
            LRESULT(0)
        }
        recorder::WM_REC_DONE => {
            let done = Box::from_raw(lp.0 as *mut recorder::Done);
            recui::on_done(*done);
            overlay::with(|o| o.resume_dup());
            LRESULT(0)
        }
        notify::WM_TEXT_COPIED => {
            let b = Box::from_raw(lp.0 as *mut (String, usize));
            notify::on_text(hwnd, b.0, b.1);
            LRESULT(0)
        }
        ocr::WM_OCR => {
            let done = Box::from_raw(lp.0 as *mut ocr::Done);
            overlay::with(|o| o.on_ocr(*done));
            LRESULT(0)
        }
        element::WM_ELEMENT => {
            let found = Box::from_raw(lp.0 as *mut element::Found);
            overlay::with(|o| o.on_element(*found));
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            let cfg = Config::load();
            overlay::with(|o| {
                o.apply_config(&cfg);
                if msg == WM_DISPLAYCHANGE {
                    o.displays_changed();
                }
            });
            recui::apply_config(&cfg);
            notify::apply_config(&cfg);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn benches(arg: &str) -> bool {
    match arg {
        "--bench-grab" => {
            for blt in [true, false] {
                let t = Instant::now();
                let f = capture::grab_with(blt);
                eprintln!("grab captureblt={blt}: {:.1} ms ({:?})", t.elapsed().as_secs_f64() * 1000.0, f.map(|f| f.bounds));
            }
        }
        "--bench-rec" => {
            let out = std::env::temp_dir().join("fastsnip-bench.mp4");
            let mons = capture::monitors();
            let m = mons.iter().find(|m| m.primary).unwrap_or(&mons[0]);
            let (tx, rx) = std::sync::mpsc::channel();
            let ctl = recorder::start_with(
                recorder::Settings { area: m.rect, fps: 60, mbps: 16, mic: true, system_audio: true, cursor: true, out: out.clone() },
                move |d| {
                    let _ = tx.send(d);
                },
            );
            ctl.go.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_secs(3));
            ctl.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let d = rx.recv().unwrap();
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            eprintln!("rec: {:?} err={:?} {:.1}s {} KB", d.path, d.error, d.seconds, size / 1024);
            let _ = std::fs::remove_file(&out);
        }
        "--bench-ocr" => {
            let f = capture::grab().expect("grab");
            let r = f.bounds;
            let (cr, bgra) = f.crop(&r).unwrap();
            let job = ocr::Job { seq: 0, kind: ocr::Kind::Monitor, origin: (cr.x, cr.y), w: cr.w as u32, h: cr.h as u32, bgra, upscale: false };
            let t = Instant::now();
            let words = ocr::read_now(&job);
            eprintln!("ocr {}x{}: {} words in {:.0} ms", cr.w, cr.h, words.len(), t.elapsed().as_secs_f64() * 1000.0);
        }
        "--make-assets" => {
            let dir = std::env::args().nth(2).unwrap_or_else(|| "Assets".into());
            match icon::make_assets(std::path::Path::new(&dir)) {
                Ok(()) => eprintln!("wrote logos to {dir}"),
                Err(e) => eprintln!("couldn't write logos: {e}"),
            }
        }
        _ => return false,
    }
    true
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let first = std::env::args().nth(1).unwrap_or_default();
    if benches(&first) {
        return;
    }
    if first == "--listener" {
        return run_listener();
    }
    let action = arg_action();
    let cfg = Config::load();

    // Reload and quit go to whichever of core and listener is running.
    if matches!(action, Some(REMOTE_RELOAD) | Some(REMOTE_QUIT)) {
        let core = find(CORE_SHARED);
        let listener = find(LISTENER_SHARED);
        for h in [core, listener].into_iter().flatten() {
            post(h, action.unwrap());
        }
        // Reload with nothing running: start in the configured mode.
        if action == Some(REMOTE_RELOAD) && core.is_none() && listener.is_none() {
            spawn_self(&[]);
        }
        return;
    }

    // Plain start in on-demand mode: just the listener.
    if action.is_none() && cfg.background == Background::OnDemand {
        if find(CORE_SHARED).is_none() {
            return run_listener();
        }
        return;
    }

    // One core per user session. A second launch forwards its request and exits.
    // A core that doesn't answer is treated as dead, so a stuck process can
    // never block FastSnip from starting.
    let _mutex = unsafe { CreateMutexW(None, true, w!(r"Local\FastSnip.Core")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if let Some(existing) = find_waiting(CORE_SHARED) {
            post(existing, action.unwrap_or(REMOTE_START));
            return;
        }
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let tray_mode = cfg.background == Background::Tray;
    hotkey::set_bindings(&cfg.shortcuts);
    let main = make_window(MAIN_CLASS, Some(main_proc));
    let _shared = publish(CORE_SHARED, main);
    overlay::register_class();
    recui::install(main, &cfg);
    notify::install(main, &cfg);
    match Overlay::new(main, &cfg) {
        Ok(o) => overlay::install(o),
        Err(e) => {
            overlay::log(&format!("graphics init failed: {e}"));
            return;
        }
    }
    if tray_mode {
        hotkey::start(main);
    } else if find(LISTENER_SHARED).is_none() {
        // On-demand mode, started directly: make sure shortcuts keep working after we quit.
        spawn_self(&["--listener"]);
    }
    set_role(main, tray_mode);
    unsafe {
        SetTimer(Some(main), IDLE_TIMER, 2000, None);
    }
    overlay::log(if tray_mode { "ready (tray)" } else { "ready (on demand)" });

    if let Some(a) = action {
        post(main, a);
    }
    run_loop();

    // Release GPU objects, the tray icon and the hook so the process ends at once.
    tray::hide(main);
    if tray_mode {
        hotkey::stop();
    }
    overlay::uninstall();
    unsafe {
        let _ = DestroyWindow(main);
    }
    overlay::log("bye");
    std::process::exit(0);
}
