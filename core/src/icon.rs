//! The FastSnip mark, drawn in code: a rounded accent square with four white
//! crop corners. Used for the tray icon (teal, or red while recording) and to
//! write the PNG logos the MSIX package needs (`fastsnip.exe --make-assets`).

use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// RGBA pixels of the mark at `size` x `size`. `rgb` is the tile color.
pub fn pixels(size: u32, rgb: u32, padding: f32) -> Vec<u8> {
    let n = size as usize;
    let mut px = vec![0u8; n * n * 4];
    let s = size as f32;
    let pad = s * padding;
    let side = s - pad * 2.0;
    let radius = side * 0.24;
    let (r, g, b) = (((rgb >> 16) & 255) as f32, ((rgb >> 8) & 255) as f32, (rgb & 255) as f32);
    // Crop corners: L shapes inset in the tile.
    let inset = side * 0.24;
    let arm = side * 0.2;
    let thick = (side * 0.085).max(1.0);
    let (lo, hi) = (pad + inset, pad + side - inset);
    let corner = |x: f32, y: f32| -> bool {
        let near = |v: f32, edge: f32, dir: f32| {
            let d = (v - edge) * dir;
            (0.0..=arm).contains(&d)
        };
        let on = |v: f32, edge: f32| (v - edge).abs() <= thick / 2.0;
        for (ex, dx) in [(lo, 1.0), (hi, -1.0)] {
            for (ey, dy) in [(lo, 1.0), (hi, -1.0)] {
                if (on(y, ey) && near(x, ex - dx * thick / 2.0, dx)) || (on(x, ex) && near(y, ey - dy * thick / 2.0, dy)) {
                    return true;
                }
            }
        }
        false
    };
    // 4x4 supersampling for smooth edges.
    for y in 0..n {
        for x in 0..n {
            let (mut tile, mut mark) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0;
                    let qx = (fx - pad).clamp(0.0, side);
                    let qy = (fy - pad).clamp(0.0, side);
                    let cx = qx.clamp(radius, side - radius);
                    let cy = qy.clamp(radius, side - radius);
                    let inside = fx >= pad && fx <= pad + side && fy >= pad && fy <= pad + side && ((qx - cx).powi(2) + (qy - cy).powi(2)) <= radius * radius;
                    if inside {
                        tile += 1;
                        if corner(fx, fy) {
                            mark += 1;
                        }
                    }
                }
            }
            let a = tile as f32 / 16.0;
            let m = if tile > 0 { mark as f32 / tile as f32 } else { 0.0 };
            let i = (y * n + x) * 4;
            px[i] = (r + (255.0 - r) * m) as u8;
            px[i + 1] = (g + (255.0 - g) * m) as u8;
            px[i + 2] = (b + (255.0 - b) * m) as u8;
            px[i + 3] = (a * 255.0) as u8;
        }
    }
    px
}

pub fn hicon(size: u32, rgb: u32) -> Option<HICON> {
    let rgba = pixels(size, rgb, 0.04);
    // Premultiplied BGRA for the color bitmap.
    let mut bgra = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        let a = p[3] as u32;
        bgra.extend_from_slice(&[(p[2] as u32 * a / 255) as u8, (p[1] as u32 * a / 255) as u8, (p[0] as u32 * a / 255) as u8, p[3]]);
    }
    unsafe {
        let color = CreateBitmap(size as i32, size as i32, 1, 32, Some(bgra.as_ptr() as *const _));
        let mask_bits = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// Write the PNG logos for the MSIX package into `dir`.
pub fn make_assets(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let teal = 0x0b7a73;
    let write = |name: &str, w: u32, h: u32, padding: f32| -> std::io::Result<()> {
        let side = w.min(h);
        let mark = pixels(side, teal, padding);
        let mut px = vec![0u8; (w * h * 4) as usize];
        let (ox, oy) = ((w - side) / 2, (h - side) / 2);
        for y in 0..side {
            for x in 0..side {
                let s = ((y * side + x) * 4) as usize;
                let d = (((y + oy) * w + x + ox) * 4) as usize;
                px[d..d + 4].copy_from_slice(&mark[s..s + 4]);
            }
        }
        let f = std::fs::File::create(dir.join(name))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().map_err(std::io::Error::other)?;
        wr.write_image_data(&px).map_err(std::io::Error::other)?;
        Ok(())
    };
    write("StoreLogo.png", 50, 50, 0.04)?;
    write("Square44x44Logo.png", 44, 44, 0.04)?;
    write("Square44x44Logo.targetsize-24_altform-unplated.png", 24, 24, 0.0)?;
    write("Square44x44Logo.targetsize-32_altform-unplated.png", 32, 32, 0.0)?;
    write("Square44x44Logo.targetsize-48_altform-unplated.png", 48, 48, 0.0)?;
    write("Square44x44Logo.targetsize-256_altform-unplated.png", 256, 256, 0.0)?;
    write("Square150x150Logo.png", 150, 150, 0.2)?;
    write("Wide310x150Logo.png", 310, 150, 0.2)?;
    write("SplashScreen.png", 620, 300, 0.25)?;
    write("AppIcon256.png", 256, 256, 0.02)?;
    Ok(())
}
