//! Tray icon for "Keep ready in the tray" mode. Left-click opens the FastSnip
//! window; right-click shows the menu. The icon turns red while recording,
//! and clicking it then stops the recording.

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hotkey::Action;

pub const WM_TRAY: u32 = WM_APP + 20;
const ID: u32 = 1;

pub const CMD_OPEN: u32 = 100;
pub const CMD_SNIP: u32 = 101;
pub const CMD_FULL: u32 = 102;
pub const CMD_RECORD: u32 = 103;
pub const CMD_TEXT: u32 = 104;
pub const CMD_SETTINGS: u32 = 105;
pub const CMD_QUIT: u32 = 106;

static mut TASKBAR_CREATED: u32 = 0;
static mut SHOWN: bool = false;
static mut RECORDING: bool = false;

pub fn taskbar_created_msg() -> u32 {
    unsafe {
        if TASKBAR_CREATED == 0 {
            TASKBAR_CREATED = RegisterWindowMessageW(w!("TaskbarCreated"));
        }
        TASKBAR_CREATED
    }
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    let tip: Vec<u16> = if unsafe { RECORDING } { "FastSnip · recording (click to stop)" } else { "FastSnip" }.encode_utf16().collect();
    d.szTip[..tip.len()].copy_from_slice(&tip);
    let color = if unsafe { RECORDING } { 0xd93636 } else { 0x0b7a73 };
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    if let Some(i) = crate::icon::hicon(size, color) {
        d.hIcon = i;
    }
    d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    d
}

pub fn show(hwnd: HWND) {
    unsafe {
        let d = data(hwnd);
        if SHOWN {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        } else {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
            SHOWN = true;
        }
        let _ = DestroyIcon(d.hIcon);
    }
}

pub fn hide(hwnd: HWND) {
    unsafe {
        if SHOWN {
            let d = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ID, ..Default::default() };
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            SHOWN = false;
        }
    }
}

/// Explorer restarted: add the icon again.
pub fn restore(hwnd: HWND) {
    unsafe {
        if SHOWN {
            SHOWN = false;
            show(hwnd);
        }
    }
}

pub fn set_recording(hwnd: HWND, on: bool) {
    unsafe {
        if RECORDING != on {
            RECORDING = on;
            if SHOWN {
                show(hwnd);
            }
        }
    }
}

/// What a tray event asks for: an action, a command id, or nothing.
pub enum Ask {
    Action(Action),
    Command(u32),
    None,
}

pub fn on_message(hwnd: HWND, lp: LPARAM) -> Ask {
    let event = (lp.0 & 0xFFFF) as u32;
    match event {
        // NIN_SELECT (WM_USER) and NIN_KEYSELECT (WM_USER + 1) with NOTIFYICON_VERSION_4.
        e if e == WM_LBUTTONUP || e == WM_USER || e == WM_USER + 1 => {
            if unsafe { RECORDING } {
                Ask::Action(Action::RecordFullScreen)
            } else {
                Ask::Command(CMD_OPEN)
            }
        }
        e if e == WM_CONTEXTMENU || e == WM_RBUTTONUP => Ask::Command(menu(hwnd)),
        _ => Ask::None,
    }
}

fn label(text: &str, key: &str) -> HSTRING {
    if key.is_empty() {
        HSTRING::from(text)
    } else {
        HSTRING::from(format!("{text}\t{key}"))
    }
}

fn menu(hwnd: HWND) -> u32 {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        let cfg = crate::config::Config::load();
        let first = |v: &Vec<String>| v.first().cloned().unwrap_or_default();
        let rec = RECORDING;
        let _ = AppendMenuW(m, MF_STRING, CMD_OPEN as usize, &HSTRING::from("Open FastSnip"));
        let _ = SetMenuDefaultItem(m, CMD_OPEN, 0);
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, CMD_SNIP as usize, &label("New snip", &first(&cfg.shortcuts.open_toolbar)));
        let _ = AppendMenuW(m, MF_STRING, CMD_FULL as usize, &label("Snip full screen", &first(&cfg.shortcuts.snip_full_screen)));
        let rec_text = if rec { "Stop recording" } else { "Record screen" };
        let _ = AppendMenuW(m, MF_STRING, CMD_RECORD as usize, &label(rec_text, &first(&cfg.shortcuts.record_full_screen)));
        let _ = AppendMenuW(m, MF_STRING, CMD_TEXT as usize, &label("Grab text", &first(&cfg.shortcuts.grab_text)));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, CMD_SETTINGS as usize, &HSTRING::from("Settings"));
        let _ = AppendMenuW(m, MF_STRING, CMD_QUIT as usize, &HSTRING::from("Quit FastSnip"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        cmd.0 as u32
    }
}
