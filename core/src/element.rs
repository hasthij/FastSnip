//! Element mode: find the UI element (button, panel, web element) under the
//! mouse with UI Automation.
//!
//! The overlay covers the screen, so UIA's own ElementFromPoint would just
//! find the overlay. Instead we start from the window under the point (from
//! the snapshot taken at freeze time) and walk down the control tree to the
//! deepest element containing the point. It runs on a worker thread and only
//! the newest request is processed, so the overlay never waits on it.

use std::sync::mpsc::{channel, Receiver, Sender};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::capture::Rect;

pub const WM_ELEMENT: u32 = WM_APP + 3;

pub struct Request {
    pub seq: u64,
    pub window: isize,
    pub x: i32,
    pub y: i32,
}

pub struct Found {
    pub seq: u64,
    pub rect: Rect,
    pub label: String,
}

pub struct Finder {
    tx: Sender<Request>,
}

impl Finder {
    pub fn start(notify: HWND) -> Self {
        let (tx, rx) = channel::<Request>();
        let target = notify.0 as isize;
        std::thread::Builder::new()
            .name("fastsnip-uia".into())
            .spawn(move || worker(rx, target))
            .expect("uia thread");
        Self { tx }
    }

    pub fn ask(&self, r: Request) {
        let _ = self.tx.send(r);
    }
}

fn worker(rx: Receiver<Request>, target: isize) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        let Ok(walker) = uia.ControlViewWalker() else {
            return;
        };
        while let Ok(mut req) = rx.recv() {
            // Skip stale requests: only the newest mouse position matters.
            while let Ok(newer) = rx.try_recv() {
                req = newer;
            }
            if let Some((rect, label)) = find(&uia, &walker, &req) {
                let msg = Box::new(Found {
                    seq: req.seq,
                    rect,
                    label,
                });
                let _ = PostMessageW(
                    Some(HWND(target as *mut _)),
                    WM_ELEMENT,
                    WPARAM(0),
                    LPARAM(Box::into_raw(msg) as isize),
                );
            }
        }
    }
}

unsafe fn rect_of(e: &IUIAutomationElement) -> Option<Rect> {
    let r = e.CurrentBoundingRectangle().ok()?;
    let r = Rect::from_win(r);
    (r.w > 0 && r.h > 0).then_some(r)
}

unsafe fn find(
    uia: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    req: &Request,
) -> Option<(Rect, String)> {
    let root = uia.ElementFromHandle(HWND(req.window as *mut _)).ok()?;
    let mut cur = root;
    let mut cur_rect = rect_of(&cur)?;
    for _ in 0..32 {
        let mut next: Option<(IUIAutomationElement, Rect)> = None;
        let mut child = walker.GetFirstChildElement(&cur).ok();
        let mut n = 0;
        while let Some(c) = child {
            if let Some(r) = rect_of(&c) {
                if r.contains(req.x, req.y) {
                    // Prefer the smallest child that holds the point.
                    let better = match &next {
                        Some((_, br)) => (r.w as i64 * r.h as i64) < (br.w as i64 * br.h as i64),
                        None => true,
                    };
                    if better {
                        next = Some((c.clone(), r));
                    }
                }
            }
            n += 1;
            if n > 400 {
                break;
            }
            child = walker.GetNextSiblingElement(&c).ok();
        }
        match next {
            Some((c, r)) => {
                cur = c;
                cur_rect = r;
            }
            None => break,
        }
    }
    let kind = cur
        .CurrentLocalizedControlType()
        .map(|b| b.to_string())
        .unwrap_or_default();
    let name = cur.CurrentName().map(|b| b.to_string()).unwrap_or_default();
    let mut label = if kind.is_empty() {
        "Element".to_string()
    } else {
        capitalize(&kind)
    };
    if !name.trim().is_empty() {
        let short: String = name.trim().chars().take(40).collect();
        label = format!("{label} · \"{short}\"");
    }
    Some((cur_rect, label))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
