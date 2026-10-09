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
    pub saving: Saving,
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
