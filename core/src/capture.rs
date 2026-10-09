//! Freezing the screen: one GDI grab of the whole virtual desktop, plus a
//! snapshot of monitors and top-level windows taken at the same moment.
//!
//! The process is per-monitor DPI aware (v2), so every coordinate here is in
//! physical pixels, which is also what the frozen bitmap uses.

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn from_win(r: RECT) -> Self {
        Self {
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
        }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then(|| Rect {
            x,
            y,
            w: r - x,
            h: b - y,
        })
    }
    pub fn from_points(ax: i32, ay: i32, bx: i32, by: i32) -> Self {
        Rect {
            x: ax.min(bx),
            y: ay.min(by),
            w: (ax - bx).abs(),
            h: (ay - by).abs(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub rect: Rect,
    pub work: Rect,
    pub dpi: u32,
    pub primary: bool,
}

impl Monitor {
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
}

#[derive(Debug, Clone)]
pub struct WinInfo {
    pub rect: Rect,
    pub title: String,
    pub hwnd: isize,
}

/// The frozen desktop. `pixels` is BGRA, top-down, `bounds.w * 4` bytes per row.
pub struct Frame {
    pub bounds: Rect,
    pub pixels: Vec<u8>,
}

impl Frame {
    /// Copy a region out as tightly packed BGRA with opaque alpha.
    pub fn crop(&self, r: &Rect) -> Option<(Rect, Vec<u8>)> {
        let r = r.intersect(&self.bounds)?;
        let stride = self.bounds.w as usize * 4;
        let mut out = Vec::with_capacity(r.w as usize * r.h as usize * 4);
        for row in 0..r.h {
            let sy = (r.y - self.bounds.y + row) as usize;
            let sx = (r.x - self.bounds.x) as usize;
            let start = sy * stride + sx * 4;
            out.extend_from_slice(&self.pixels[start..start + r.w as usize * 4]);
        }
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Some((r, out))
    }

    pub fn pixel(&self, x: i32, y: i32) -> Option<[u8; 3]> {
        if !self.bounds.contains(x, y) {
            return None;
        }
        let i = ((y - self.bounds.y) as usize * self.bounds.w as usize
            + (x - self.bounds.x) as usize)
            * 4;
        Some([self.pixels[i + 2], self.pixels[i + 1], self.pixels[i]])
    }
}

pub fn virtual_screen() -> Rect {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// Grab every monitor in one BitBlt. ~5-20 ms for a 1440p-4K desktop.
pub fn grab() -> Option<Frame> {
    grab_with(true)
}

pub fn grab_with(captureblt: bool) -> Option<Frame> {
    let b = virtual_screen();
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = b.w;
        bmi.bmiHeader.biHeight = -b.h; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(d) => d,
            Err(_) => {
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return None;
            }
        };
        let old = SelectObject(mem, dib.into());
        let ok = BitBlt(
            mem,
            0,
            0,
            b.w,
            b.h,
            Some(screen),
            b.x,
            b.y,
            if captureblt {
                SRCCOPY | CAPTUREBLT
            } else {
                SRCCOPY
            },
        )
        .is_ok();
        let len = b.w as usize * b.h as usize * 4;
        let pixels = if ok {
            std::slice::from_raw_parts(bits as *const u8, len).to_vec()
        } else {
            Vec::new()
        };
        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        ok.then(|| Frame { bounds: b, pixels })
    }
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(h, &mut mi).as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            list.push(Monitor {
                rect: Rect::from_win(mi.rcMonitor),
                work: Rect::from_win(mi.rcWork),
                dpi: dx,
                primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        true.into()
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// Visible top-level windows, front to back, with DWM frame bounds (no drop shadow).
pub fn windows(skip_pid: u32) -> Vec<WinInfo> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        let (list, skip) = &mut *(data.0 as *mut (Vec<WinInfo>, u32));
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == *skip {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
        if cloaked != 0 {
            return true.into();
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            return true.into();
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            let _ = GetWindowRect(h, &mut r);
        }
        let rect = Rect::from_win(r);
        if rect.w < 8 || rect.h < 8 {
            return true.into();
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        list.push(WinInfo {
            rect,
            title,
            hwnd: h.0 as isize,
        });
        true.into()
    }
    let mut data: (Vec<WinInfo>, u32) = (Vec::new(), skip_pid);
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut data as *mut _ as isize));
    }
    data.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_math() {
        let a = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let b = Rect {
            x: 50,
            y: 50,
            w: 100,
            h: 100,
        };
        assert_eq!(
            a.intersect(&b),
            Some(Rect {
                x: 50,
                y: 50,
                w: 50,
                h: 50
            })
        );
        assert_eq!(
            Rect::from_points(10, 20, 5, 2),
            Rect {
                x: 5,
                y: 2,
                w: 5,
                h: 18
            }
        );
    }

    #[test]
    fn crop_sets_alpha() {
        let f = Frame {
            bounds: Rect {
                x: -10,
                y: 0,
                w: 4,
                h: 2,
            },
            pixels: vec![7; 4 * 2 * 4],
        };
        let (r, px) = f
            .crop(&Rect {
                x: -9,
                y: 0,
                w: 2,
                h: 2,
            })
            .unwrap();
        assert_eq!(r.w, 2);
        assert_eq!(px.len(), 16);
        assert!(px.chunks(4).all(|p| p[3] == 255 && p[0] == 7));
    }
}
