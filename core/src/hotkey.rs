//! Global shortcuts through a low-level keyboard hook (WH_KEYBOARD_LL).
//!
//! The hook runs on its own thread so rendering can never delay it past the
//! system's hook timeout. The callback only matches keys and posts a message
//! to the main window; all real work happens there.

use std::collections::HashSet;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Shortcuts;

pub const WM_HOTKEY_ACTION: u32 = WM_APP + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Action {
    OpenToolbar = 1,
    SnipFullScreen = 2,
    RecordFullScreen = 3,
    GrabText = 4,
}

impl Action {
    pub fn from_usize(v: usize) -> Option<Self> {
        match v {
            1 => Some(Self::OpenToolbar),
            2 => Some(Self::SnipFullScreen),
            3 => Some(Self::RecordFullScreen),
            4 => Some(Self::GrabText),
            _ => None,
        }
    }
}

const MOD_WIN: u8 = 1;
const MOD_CTRL: u8 = 2;
const MOD_SHIFT: u8 = 4;
const MOD_ALT: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Binding {
    mods: u8,
    vk: u32,
    action: Action,
}

static BINDINGS: OnceLock<Mutex<Vec<Binding>>> = OnceLock::new();
static HELD: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
static TARGET: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Unassigned virtual key, sent after swallowing a Win+ combo so the Start menu doesn't open.
const VK_DUMMY: u16 = 0xE8;

/// Parse "Win+Shift+S", "PrintScreen", "Ctrl+Alt+F5" and similar.
pub fn parse(spec: &str) -> Option<(u8, u32)> {
    let mut mods = 0u8;
    let mut key = None;
    for part in spec.split('+').map(|p| p.trim()) {
        match part.to_ascii_lowercase().as_str() {
            "win" | "windows" | "meta" => mods |= MOD_WIN,
            "ctrl" | "control" => mods |= MOD_CTRL,
            "shift" => mods |= MOD_SHIFT,
            "alt" => mods |= MOD_ALT,
            other => key = vk_from_name(other),
        }
    }
    key.map(|k| (mods, k))
}

fn vk_from_name(n: &str) -> Option<u32> {
    let vk = match n {
        "printscreen" | "prtsc" | "print" | "prtscn" => VK_SNAPSHOT.0 as u32,
        "space" => VK_SPACE.0 as u32,
        "insert" | "ins" => VK_INSERT.0 as u32,
        "pause" => VK_PAUSE.0 as u32,
        "scrolllock" => VK_SCROLL.0 as u32,
        "home" => VK_HOME.0 as u32,
        "end" => VK_END.0 as u32,
        _ => {
            let b = n.as_bytes();
            if b.len() == 1 && (b[0].is_ascii_alphanumeric()) {
                b[0].to_ascii_uppercase() as u32
            } else if let Some(num) = n.strip_prefix('f').and_then(|x| x.parse::<u32>().ok()) {
                if (1..=24).contains(&num) {
                    VK_F1.0 as u32 + num - 1
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    };
    Some(vk)
}

/// Replace the active bindings (safe to call while the hook is running).
pub fn set_bindings(s: &Shortcuts) {
    let mut v = Vec::new();
    let mut add = |list: &Vec<String>, action: Action| {
        for spec in list {
            if let Some((mods, vk)) = parse(spec) {
                v.push(Binding { mods, vk, action });
            }
        }
    };
    add(&s.open_toolbar, Action::OpenToolbar);
    add(&s.snip_full_screen, Action::SnipFullScreen);
    add(&s.record_full_screen, Action::RecordFullScreen);
    add(&s.grab_text, Action::GrabText);
    *BINDINGS.get_or_init(Default::default).lock().unwrap() = v;
    // Re-register the system hotkeys on the hook thread (they belong to that thread).
    let id = HOOK_THREAD.load(Ordering::SeqCst);
    if id != 0 {
        unsafe {
            let _ = PostThreadMessageW(id, WM_REREGISTER, WPARAM(0), LPARAM(0));
        }
    }
}

const WM_REREGISTER: u32 = WM_APP + 30;
const HOTKEY_BASE: i32 = 0x4F00;

/// System hotkeys for every binding Windows lets us register.
///
/// The low-level hook can't see keys while an app running as administrator
/// (Task Manager, an elevated terminal) is in front: Windows' UI privilege
/// isolation hides them. Registered hotkeys are delivered anyway. In normal
/// use the hook swallows the key first, so the hotkey never fires twice.
/// Combos Windows keeps for itself (like Win+Shift+S) just fail to register.
unsafe fn register_hotkeys() -> i32 {
    let list: Vec<Binding> = BINDINGS.get().map(|b| b.lock().unwrap().clone()).unwrap_or_default();
    let mut n = 0;
    for (i, b) in list.iter().enumerate() {
        let mut m = MOD_NOREPEAT;
        if b.mods & MOD_WIN != 0 {
            m |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_WIN;
        }
        if b.mods & MOD_CTRL != 0 {
            m |= MOD_CONTROL;
        }
        if b.mods & MOD_SHIFT != 0 {
            m |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT;
        }
        if b.mods & MOD_ALT != 0 {
            m |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_ALT;
        }
        match RegisterHotKey(None, HOTKEY_BASE + i as i32, m, b.vk) {
            Ok(()) => n += 1,
            Err(e) => crate::overlay::log(&format!("system hotkey {:#x}+{:#x} not registered: {e}", b.mods, b.vk)),
        }
    }
    crate::overlay::log(&format!("registered {n} of {} system hotkeys", list.len()));
    list.len() as i32
}

unsafe fn unregister_hotkeys(count: i32) {
    for i in 0..count {
        let _ = UnregisterHotKey(None, HOTKEY_BASE + i);
    }
}

/// Start the hook thread. Matching shortcuts post WM_HOTKEY_ACTION to `target`.
pub fn start(target: HWND) {
    TARGET.store(target.0 as isize, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("fastsnip-hook".into())
        .spawn(|| unsafe {
            HOOK_THREAD.store(
                windows::Win32::System::Threading::GetCurrentThreadId(),
                Ordering::SeqCst,
            );
            let hmod = GetModuleHandleW(None).ok();
            let hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hmod.map(|m| m.into()), 0);
            let mut registered = register_hotkeys();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.hwnd.is_invalid() && msg.message == WM_HOTKEY {
                    // Delivered only when the hook couldn't see the key (admin window in front).
                    let idx = msg.wParam.0 as i32 - HOTKEY_BASE;
                    crate::overlay::log(&format!("system hotkey {idx} pressed"));
                    let action = BINDINGS.get().and_then(|b| b.lock().unwrap().get(idx.max(0) as usize).map(|b| b.action));
                    if let Some(a) = action {
                        let target = HWND(TARGET.load(Ordering::SeqCst) as *mut _);
                        let _ = PostMessageW(Some(target), WM_HOTKEY_ACTION, WPARAM(a as usize), LPARAM(0));
                    }
                    continue;
                }
                if msg.hwnd.is_invalid() && msg.message == WM_REREGISTER {
                    unregister_hotkeys(registered);
                    registered = register_hotkeys();
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            unregister_hotkeys(registered);
            if let Ok(h) = hook {
                let _ = UnhookWindowsHookEx(h);
            }
        })
        .expect("hook thread");
}

/// Remove the hook and end its thread.
pub fn stop() {
    let id = HOOK_THREAD.load(Ordering::SeqCst);
    if id != 0 {
        unsafe {
            let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn current_mods() -> u8 {
    unsafe {
        let down = |vk: VIRTUAL_KEY| (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0;
        let mut m = 0;
        if down(VK_LWIN) || down(VK_RWIN) {
            m |= MOD_WIN;
        }
        if down(VK_CONTROL) {
            m |= MOD_CTRL;
        }
        if down(VK_SHIFT) {
            m |= MOD_SHIFT;
        }
        if down(VK_MENU) {
            m |= MOD_ALT;
        }
        m
    }
}

/// Inject an unassigned key. Also makes Windows treat us as the source of the
/// last input, which lets the overlay take focus without AttachThreadInput.
pub fn send_dummy_key() {
    let mk = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_DUMMY),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [mk(KEYBD_EVENT_FLAGS(0)), mk(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0;
        if !injected {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            let vk = kb.vkCode;
            let held = HELD.get_or_init(Default::default);

            if up && held.lock().unwrap().remove(&vk) {
                return LRESULT(1);
            }
            if down {
                let mods = current_mods();
                let hit = BINDINGS.get().and_then(|b| {
                    b.lock()
                        .unwrap()
                        .iter()
                        .find(|b| b.vk == vk && b.mods == mods)
                        .copied()
                });
                if let Some(b) = hit {
                    let first = held.lock().unwrap().insert(vk);
                    if first {
                        if b.mods & MOD_WIN != 0 {
                            send_dummy_key();
                        }
                        let target = HWND(TARGET.load(Ordering::SeqCst) as *mut _);
                        let _ = PostMessageW(
                            Some(target),
                            WM_HOTKEY_ACTION,
                            WPARAM(b.action as usize),
                            LPARAM(0),
                        );
                    }
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        assert_eq!(parse("PrintScreen"), Some((0, VK_SNAPSHOT.0 as u32)));
        assert_eq!(
            parse("Win+Shift+S"),
            Some((MOD_WIN | MOD_SHIFT, b'S' as u32))
        );
        assert_eq!(
            parse("ctrl+alt+f5"),
            Some((MOD_CTRL | MOD_ALT, VK_F5.0 as u32))
        );
        assert_eq!(parse("Win+Shift+"), None);
    }
}
