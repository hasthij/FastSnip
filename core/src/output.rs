//! Where captures go: the clipboard right away, then a PNG on disk.
//!
//! Order matters for speed. The clipboard gets a DIB synchronously (a memcpy),
//! so pasting works the instant the overlay closes. PNG encoding and the file
//! write happen on a worker thread, and the PNG is then added to the clipboard
//! as a second format so apps that prefer PNG (and transparency) get it.

use std::path::{Path, PathBuf};

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{
    FOLDERID_Screenshots, FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_CREATE,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_PNG_READY: u32 = WM_APP + 2;

static PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Screenshots still being encoded or written.
pub fn pending() -> usize {
    PENDING.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn done_saving() {
    let _ = PENDING.fetch_update(std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst, |v| v.checked_sub(1));
}

/// A finished capture: tightly packed BGRA. Alpha is 0 outside a freeform shape.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// Sent back to the main thread once the PNG exists.
pub struct PngReady {
    pub png: Vec<u8>,
    pub path: Option<PathBuf>,
    pub image: std::sync::Arc<Image>,
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let p: PWSTR = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

pub fn screenshots_dir(custom: &str) -> PathBuf {
    if !custom.is_empty() {
        return PathBuf::from(custom);
    }
    known_folder(&FOLDERID_Screenshots)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|u| PathBuf::from(u).join("Pictures").join("Screenshots"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn recordings_dir(custom: &str) -> PathBuf {
    if !custom.is_empty() {
        return PathBuf::from(custom);
    }
    known_folder(&FOLDERID_Videos)
        .map(|v| v.join("Screen Recordings"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// "Screenshot 2026-10-09 143207.png", the same pattern Windows uses.
pub fn timestamp_name(prefix: &str, ext: &str) -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{prefix} {:04}-{:02}-{:02} {:02}{:02}{:02}.{ext}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}).{ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

pub fn encode_png(img: &Image) -> Option<Vec<u8>> {
    let mut rgba = img.bgra.clone();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let mut out = Vec::with_capacity(rgba.len() / 3);
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&rgba).ok()?;
    }
    Some(out)
}

unsafe fn global_from(bytes: &[u8]) -> Option<HGLOBAL> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        let _ = GlobalFree(Some(h));
        return None;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    let _ = GlobalUnlock(h);
    Some(h)
}

/// 32-bit bottom-up DIB. Transparent pixels (freeform) become white so every app shows it right.
fn dib_bytes(img: &Image) -> Vec<u8> {
    let hdr = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: img.w as i32,
        biHeight: img.h as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: img.w * img.h * 4,
        ..Default::default()
    };
    let hdr_bytes = unsafe {
        std::slice::from_raw_parts(
            &hdr as *const _ as *const u8,
            std::mem::size_of::<BITMAPINFOHEADER>(),
        )
    };
    let mut v = Vec::with_capacity(hdr_bytes.len() + img.bgra.len());
    v.extend_from_slice(hdr_bytes);
    let row = img.w as usize * 4;
    for y in (0..img.h as usize).rev() {
        for px in img.bgra[y * row..(y + 1) * row].chunks_exact(4) {
            if px[3] == 0 {
                v.extend_from_slice(&[255, 255, 255, 255]);
            } else {
                v.extend_from_slice(px);
            }
        }
    }
    v
}

/// Put the image on the clipboard now. Returns false if the clipboard was busy.
pub fn clipboard_set_image(owner: HWND, img: &Image) -> bool {
    let dib = dib_bytes(img);
    unsafe {
        for _ in 0..10 {
            if OpenClipboard(Some(owner)).is_ok() {
                let _ = EmptyClipboard();
                let ok = match global_from(&dib) {
                    Some(h) => SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(h.0))).is_ok(),
                    None => false,
                };
                let _ = CloseClipboard();
                return ok;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    false
}

/// Add the PNG as a second clipboard format, only if our image is still on the clipboard.
pub fn clipboard_add_png(owner: HWND, png: &[u8]) {
    unsafe {
        if GetClipboardOwner().ok() != Some(owner) {
            return;
        }
        if OpenClipboard(Some(owner)).is_ok() {
            let fmt = RegisterClipboardFormatW(w!("PNG"));
            if let Some(h) = global_from(png) {
                let _ = SetClipboardData(fmt, Some(HANDLE(h.0)));
            }
            let _ = CloseClipboard();
        }
    }
}

pub fn clipboard_set_text(owner: HWND, text: &str) -> bool {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    unsafe {
        if OpenClipboard(Some(owner)).is_ok() {
            let _ = EmptyClipboard();
            let ok = match global_from(bytes) {
                Some(h) => SetClipboardData(13 /* CF_UNICODETEXT */, Some(HANDLE(h.0))).is_ok(),
                None => false,
            };
            let _ = CloseClipboard();
            return ok;
        }
    }
    false
}

/// Encode and save on a worker thread, then notify `notify` with WM_PNG_READY.
pub fn save_async(img: std::sync::Arc<Image>, dir: PathBuf, notify: HWND) {
    let target = notify.0 as isize;
    PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(move || {
        let Some(png) = encode_png(&img) else {
            done_saving();
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = unique_path(&dir, &timestamp_name("Screenshot", "png"));
        let saved = std::fs::write(&path, &png).is_ok().then_some(path);
        let msg = Box::new(PngReady { png, path: saved, image: img });
        unsafe {
            let _ = PostMessageW(
                Some(HWND(target as *mut _)),
                WM_PNG_READY,
                WPARAM(0),
                LPARAM(Box::into_raw(msg) as isize),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_size() {
        let img = Image {
            w: 3,
            h: 2,
            bgra: vec![10, 20, 30, 255].repeat(6),
        };
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn dib_is_bottom_up_with_white_for_transparent() {
        let mut bgra = vec![0u8; 2 * 2 * 4];
        bgra[0..4].copy_from_slice(&[1, 2, 3, 255]); // top-left opaque
        let img = Image { w: 2, h: 2, bgra };
        let d = dib_bytes(&img);
        let px = &d[40..];
        // bottom row first: both transparent -> white
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        // top row comes last; its first pixel is the opaque one
        assert_eq!(&px[8..12], &[1, 2, 3, 255]);
    }
}
