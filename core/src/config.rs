//! User settings, stored as TOML in %APPDATA%\FastSnip\config.toml.
//! The WinUI app edits the same file; the core reloads it when told to.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub background: Background,
    pub shortcuts: Shortcuts,
    pub look: Look,
    pub text: TextSettings,
    pub recording: Recording,
    pub saving: Saving,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Recording {
    /// 30, 60 or 120.
    pub fps: u32,
    /// "low", "standard" or "high".
    pub quality: String,
    pub microphone: bool,
    pub system_audio: bool,
    pub show_cursor: bool,
    /// Seconds before recording starts: 0, 3 or 5.
    pub countdown: u32,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            fps: 60,
            quality: "high".into(),
            microphone: true,
            system_audio: true,
            show_cursor: true,
            countdown: 3,
        }
    }
}

impl Recording {
    /// Bitrate in Mbps for an area, from the quality setting (bits per pixel per frame).
    pub fn mbps(&self, w: i32, h: i32) -> u32 {
        let bpp = match self.quality.as_str() {
            "low" => 0.03,
            "standard" => 0.06,
            _ => 0.1,
        };
        let bits = w as f64 * h as f64 * self.fps as f64 * bpp;
        ((bits / 1_000_000.0).round() as u32).clamp(2, 120)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Background {
    /// Stay loaded with a tray icon. Opens in ~15 ms.
    Tray,
    /// Only a tiny key listener stays running. Opens in ~100-200 ms.
    OnDemand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub open_toolbar: Vec<String>,
    pub snip_full_screen: Vec<String>,
    pub record_full_screen: Vec<String>,
    pub grab_text: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    /// "teal", "blue", "violet", "green", "orange", "pink", "system" or "#RRGGBB".
    pub accent: String,
    /// "system", "light" or "dark".
    pub theme: String,
    pub magnifier: bool,
    /// Toolbar pop-in and dim fade. Our own setting: Windows' "Animation effects" doesn't affect it.
    pub animations: bool,
    /// 0.5 to 3.0. Higher is faster: durations are divided by this.
    pub animation_speed: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TextSettings {
    /// "auto", "text-recognizer" or "media-ocr".
    pub engine: String,
    pub read_on_freeze: bool,
    pub keep_line_breaks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Saving {
    /// Empty means the Windows Screenshots folder.
    pub screenshots_dir: String,
    /// Empty means Videos\Screen Recordings.
    pub recordings_dir: String,
    pub show_toast: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            background: Background::Tray,
            shortcuts: Shortcuts::default(),
            look: Look::default(),
            text: TextSettings::default(),
            recording: Recording::default(),
            saving: Saving::default(),
        }
    }
}

impl Default for Background {
    fn default() -> Self {
        Background::Tray
    }
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            open_toolbar: vec!["PrintScreen".into(), "Win+Shift+S".into()],
            snip_full_screen: vec![],
            record_full_screen: vec!["Win+Shift+R".into()],
            grab_text: vec![],
        }
    }
}

impl Default for Look {
    fn default() -> Self {
        Self {
            accent: "teal".into(),
            theme: "system".into(),
            magnifier: true,
            animations: true,
            animation_speed: 1.0,
        }
    }
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            engine: "auto".into(),
            read_on_freeze: true,
            keep_line_breaks: false,
        }
    }
}

impl Default for Saving {
    fn default() -> Self {
        Self {
            screenshots_dir: String::new(),
            recordings_dir: String::new(),
            show_toast: true,
        }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("FastSnip")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

impl Config {
    /// Load the config, writing defaults on first run. A broken file falls back to defaults.
    pub fn load() -> Self {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let c = Config::default();
                c.save();
                c
            }
        }
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path(), s);
        }
    }
}
