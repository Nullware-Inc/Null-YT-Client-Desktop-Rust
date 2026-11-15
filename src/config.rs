//! User settings, persisted as JSON in the OS config dir.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", content = "value")]
pub enum Account {
    SignedOut,
    /// Reuse a signed-in browser session (firefox, chrome, edge, brave...).
    Browser(String),
    /// cookies.txt copied into the app data dir.
    CookieFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // Look
    pub glass: bool,
    pub dark: bool,
    pub accent: String,
    pub radius: f32,
    pub tint: f32,
    pub rim: f32,
    pub aurora: bool,
    /// "none" | "mica" | "acrylic" (Windows)
    pub window_effect: String,
    pub columns: i32, // 0 = auto
    // Feed
    pub hide_shorts: bool,
    pub hide_live: bool,
    pub blocked_keywords: Vec<String>,
    pub feed_size: u32,
    // Player
    pub quality: u32,
    pub player: String,
    // Sync
    pub account: Account,
    pub send_watch_history: bool,
    // Engine
    pub ytdlp_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            glass: true,
            dark: true,
            accent: "#ff3d5a".into(),
            radius: 18.0,
            tint: 0.10,
            rim: 0.28,
            aurora: true,
            window_effect: "none".into(),
            columns: 0,
            hide_shorts: false,
            hide_live: false,
            blocked_keywords: vec![],
            feed_size: 60,
            quality: 1080,
            player: "mpv".into(),
            account: Account::SignedOut,
            send_watch_history: true,
            ytdlp_path: None,
        }
    }
}

pub fn data_dir() -> PathBuf {
    let d = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("NullYT");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn config_path() -> PathBuf {
    data_dir().join("settings.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), s);
        }
    }

    pub fn cookie_file() -> PathBuf {
        data_dir().join("youtube_cookies.txt")
    }
}
