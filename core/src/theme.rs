//! Colors from the design spec (docs/design/fastsnip-design.html), resolved
//! for the current Windows theme and the accent setting.

use windows::core::w;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub accent: D2D1_COLOR_F,
    pub accent_soft: D2D1_COLOR_F,
    pub on_accent: D2D1_COLOR_F,
    pub rec: D2D1_COLOR_F,
    pub fg: D2D1_COLOR_F,
    pub muted: D2D1_COLOR_F,
    pub bar: D2D1_COLOR_F,
    pub bar_line: D2D1_COLOR_F,
    pub hover: D2D1_COLOR_F,
    pub seg_on: D2D1_COLOR_F,
    pub dim: D2D1_COLOR_F,
    pub ocr: D2D1_COLOR_F,
    pub ocr_sel: D2D1_COLOR_F,
}

pub const fn rgb(hex: u32) -> D2D1_COLOR_F {
    rgba(hex, 1.0)
}

pub const fn rgba(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 255) as f32 / 255.0,
        g: ((hex >> 8) & 255) as f32 / 255.0,
        b: (hex & 255) as f32 / 255.0,
        a,
    }
}

pub fn with_alpha(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a, ..c }
}

fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

fn luminance(c: D2D1_COLOR_F) -> f32 {
    let f = |v: f32| {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
}

fn reg_dword(key: windows::core::PCWSTR, value: windows::core::PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut _ as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(data)
}

pub fn system_is_dark() -> bool {
    reg_dword(
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("AppsUseLightTheme"),
    )
    .map(|v| v == 0)
    .unwrap_or(false)
}

/// Windows accent color (stored as 0xAABBGGRR).
fn system_accent() -> Option<u32> {
    reg_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"))
        .map(|abgr| ((abgr & 0xFF) << 16) | (abgr & 0xFF00) | ((abgr >> 16) & 0xFF))
}

fn preset(name: &str, dark: bool) -> Option<u32> {
    let (l, d) = match name {
        "teal" => (0x0b7a73, 0x3cc4b9),
        "blue" => (0x0067c0, 0x4cc2ff),
        "violet" => (0x6b4eff, 0xa594ff),
        "green" => (0x1d7f3a, 0x5fd08f),
        "orange" => (0xc25400, 0xff9a4d),
        "pink" => (0xc2185b, 0xff7fb0),
        _ => return None,
    };
    Some(if dark { d } else { l })
}

pub fn palette(accent_setting: &str, theme_setting: &str) -> Palette {
    let dark = match theme_setting {
        "dark" => true,
        "light" => false,
        _ => system_is_dark(),
    };
    let white = rgb(0xffffff);
    let mut accent = if let Some(hex) = preset(accent_setting, dark) {
        rgb(hex)
    } else if accent_setting == "system" {
        rgb(system_accent().unwrap_or(if dark { 0x4cc2ff } else { 0x0067c0 }))
    } else if let Some(hex) = accent_setting
        .strip_prefix('#')
        .and_then(|h| u32::from_str_radix(h, 16).ok())
    {
        rgb(hex)
    } else {
        rgb(preset("teal", dark).unwrap())
    };
    // Keep custom and system accents readable on dark surfaces.
    if dark && luminance(accent) < 0.2 && preset(accent_setting, dark).is_none() {
        accent = mix(accent, white, 0.3);
    }
    let on_accent = if luminance(accent) > 0.4 {
        rgb(0x06201d)
    } else {
        white
    };
    if dark {
        let panel = rgb(0x151c1c);
        Palette {
            dark,
            accent,
            accent_soft: mix(panel, accent, 0.16),
            on_accent,
            rec: rgb(0xff5c5c),
            fg: rgb(0xe6eded),
            muted: rgb(0x93a3a3),
            bar: rgba(0x1c2424, 0.97),
            bar_line: rgb(0x334141),
            hover: rgb(0x253131),
            seg_on: rgb(0x151c1c),
            dim: rgba(0x000000, 0.52),
            ocr: with_alpha(accent, 0.20),
            ocr_sel: with_alpha(accent, 0.45),
        }
    } else {
        Palette {
            dark,
            accent,
            accent_soft: mix(white, accent, 0.16),
            on_accent,
            rec: rgb(0xd93636),
            fg: rgb(0x121a1a),
            muted: rgb(0x566565),
            bar: rgba(0xfbfdfd, 0.97),
            bar_line: rgb(0xcbd7d7),
            hover: rgb(0xe8eeee),
            seg_on: white,
            dim: rgba(0x061010, 0.46),
            ocr: with_alpha(accent, 0.20),
            ocr_sel: with_alpha(accent, 0.45),
        }
    }
}

/// "#RRGGBB" for an SVG fill/stroke.
pub fn css(c: D2D1_COLOR_F) -> String {
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(c.r), to(c.g), to(c.b))
}
