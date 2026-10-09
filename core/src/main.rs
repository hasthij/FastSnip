//! FastSnip core.
//!
//! Stays loaded (tray mode) so the overlay opens in one frame. A low-level
//! keyboard hook catches the shortcuts; the overlay, capture and output run
//! on the main thread, with PNG encoding and UI Automation on workers.
//!
//! Command line:
//!   fastsnip.exe            start (or, if already running, do nothing)
//!   fastsnip.exe --open     open the toolbar (starts the core if needed)
//!   fastsnip.exe --text     open the toolbar in Text
//!   fastsnip.exe --full     full screen shot of the display under the mouse
//!   fastsnip.exe --quit     stop the running core

#![cfg_attr(
    all(not(debug_assertions), not(feature = "console")),
    windows_subsystem = "windows"
)]

mod capture;
mod config;
mod dup;
mod element;
mod gfx;
mod hotkey;
mod output;
mod overlay;
mod theme;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::hotkey::{Action, WM_HOTKEY_ACTION};
use crate::overlay::Overlay;

const MAIN_CLASS: windows::core::PCWSTR = w!("FastSnipCore");
/// Posted by a second instance or the app window: wparam = Action, or 0 to quit, 99 to reload config.
const WM_REMOTE: u32 = WM_APP + 10;
const REMOTE_QUIT: usize = 0;
const REMOTE_RELOAD: usize = 99;
const REMOTE_CLOSE: usize = 98;

fn responsive_core() -> Option<HWND> {
    let me = std::process::id();
    let mut after: Option<HWND> = None;
    unsafe {
        while let Ok(h) = FindWindowExW(Some(HWND_MESSAGE), after, MAIN_CLASS, None) {
            after = Some(h);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == me {
                continue;
            }
            let mut res = 0usize;
            if SendMessageTimeoutW(
                h,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                200,
                Some(&mut res),
            )
            .0 != 0
            {
                return Some(h);
            }
        }
    }
    None
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

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY_ACTION | WM_REMOTE => {
            if msg == WM_REMOTE && wp.0 == REMOTE_QUIT {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            if msg == WM_REMOTE && wp.0 == REMOTE_RELOAD {
                let cfg = Config::load();
                hotkey::set_bindings(&cfg.shortcuts);
                overlay::with(|o| o.apply_config(&cfg));
                return LRESULT(0);
            }
            if msg == WM_REMOTE && wp.0 == REMOTE_CLOSE {
                overlay::with(|o| o.close());
                return LRESULT(0);
            }
            if let Some(a) = Action::from_usize(wp.0) {
                overlay::with(|o| o.on_action(a));
            }
            LRESULT(0)
        }
        output::WM_PNG_READY => {
            let ready = Box::from_raw(lp.0 as *mut output::PngReady);
            output::clipboard_add_png(hwnd, &ready.png);
            if let Some(p) = &ready.path {
                overlay::log(&format!("saved {}", p.display()));
            }
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
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let action = arg_action();
    if std::env::args().nth(1).as_deref() == Some("--bench-grab") {
        for blt in [true, false, true, false] {
            let t = std::time::Instant::now();
            let f = capture::grab_with(blt);
            eprintln!(
                "grab captureblt={blt}: {:.1} ms ({:?})",
                t.elapsed().as_secs_f64() * 1000.0,
                f.map(|f| f.bounds)
            );
        }
        return;
    }

    // One core per user session. A second launch forwards its request to the
    // running core and exits. A core that doesn't answer within 200 ms is
    // treated as dead, so a stuck process can never block FastSnip from starting.
    let _mutex = unsafe { CreateMutexW(None, true, w!(r"Local\FastSnip.Core")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if let Some(existing) = responsive_core() {
            if let Some(a) = action {
                unsafe {
                    let _ = PostMessageW(Some(existing), WM_REMOTE, WPARAM(a), LPARAM(0));
                }
            }
            return;
        }
    }
    if action == Some(REMOTE_QUIT) {
        return;
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let cfg = Config::load();
    hotkey::set_bindings(&cfg.shortcuts);

    let main = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: hinst.into(),
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            MAIN_CLASS,
            &HSTRING::from("FastSnip"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        )
        .expect("main window")
    };

    overlay::register_class();
    match Overlay::new(main, &cfg) {
        Ok(o) => overlay::install(o),
        Err(e) => {
            overlay::log(&format!("graphics init failed: {e}"));
            return;
        }
    }
    hotkey::start(main);
    overlay::log("ready");

    if let Some(a) = action {
        unsafe {
            let _ = PostMessageW(Some(main), WM_REMOTE, WPARAM(a), LPARAM(0));
        }
    }

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Release GPU objects and the hook before exiting so the process ends at once.
    overlay::log("quitting");
    hotkey::stop();
    overlay::log("hook stopped");
    overlay::uninstall();
    overlay::log("overlay released");
    unsafe {
        let _ = DestroyWindow(main);
    }
    overlay::log("bye");
    std::process::exit(0);
}
