//! yt-dlp engine: manages its own yt-dlp binary and turns its JSON into
//! [`VideoItem`]s. Same command lines as the Dart core, so both clients get
//! the same YouTube sync behaviour.

use crate::config::{Account, Config};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // WatchLater/Liked: library tabs, next milestone
pub enum Feed {
    Recommended,
    Subscriptions,
    History,
    WatchLater,
    Liked,
}

impl Feed {
    pub fn key(self) -> &'static str {
        match self {
            Feed::Recommended => ":ytrec",
            Feed::Subscriptions => ":ytsubs",
            Feed::History => ":ythis",
            Feed::WatchLater => ":ytwatchlater",
            Feed::Liked => ":ytfav",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoItem {
    pub id: String,
    pub title: String,
    pub channel: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    pub thumbnail: String,
    pub duration: Option<f64>,
    pub views: Option<u64>,
    pub live: bool,
    pub short: bool,
}

impl VideoItem {
    fn from_json(v: &Value) -> Option<Self> {
        let id = v.get("id")?.as_str()?.to_string();
        let url = v.get("url").and_then(Value::as_str).unwrap_or_default();
        let thumbnail = v
            .get("thumbnails")
            .and_then(Value::as_array)
            .and_then(|t| {
                t.iter()
                    .filter(|x| x.get("url").is_some())
                    .max_by_key(|x| x.get("width").and_then(Value::as_u64).unwrap_or(0))
            })
            .and_then(|x| x.get("url")?.as_str().map(String::from))
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));
        Some(Self {
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled")
                .into(),
            channel: v
                .get("channel")
                .or_else(|| v.get("uploader"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            channel_id: v
                .get("channel_id")
                .and_then(Value::as_str)
                .map(String::from),
            duration: v.get("duration").and_then(Value::as_f64),
            views: v
                .get("view_count")
                .or_else(|| v.get("concurrent_view_count"))
                .and_then(Value::as_u64),
            live: v.get("live_status").and_then(Value::as_str) == Some("is_live"),
            short: url.contains("/shorts/"),
            thumbnail,
            id,
        })
    }

    /// Groups videos by channel (id when known, else the name).
    pub fn channel_key(&self) -> String {
        self.channel_id
            .clone()
            .unwrap_or_else(|| self.channel.clone())
    }

    pub fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
}

/// yt-dlp itself, embedded at build time (see build.rs).
static BUNDLED_YTDLP: &[u8] = include_bytes!(env!("NULLYT_YTDLP_PATH"));

pub struct Engine;

impl Engine {
    fn managed_path() -> PathBuf {
        crate::config::data_dir()
            .join("bin")
            .join(if cfg!(windows) {
                "yt-dlp.exe"
            } else {
                "yt-dlp"
            })
    }

    pub fn exe(cfg: &Config) -> PathBuf {
        cfg.ytdlp_path
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(Self::managed_path)
    }

    /// Writes the yt-dlp baked into this binary to the data dir on first run.
    /// It lives there (not next to the exe) so `yt-dlp -U` can update it.
    pub fn ensure_installed(cfg: &Config) -> Result<(), String> {
        let path = Self::exe(cfg);
        if path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, BUNDLED_YTDLP).map_err(|e| format!("install yt-dlp: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
        Ok(())
    }

    pub fn update(cfg: &Config) {
        let _ = Self::command(cfg).arg("-U").output();
    }

    fn command(cfg: &Config) -> Command {
        let mut c = Command::new(Self::exe(cfg));
        c.stdin(Stdio::null());
        #[cfg(windows)]
        c.creation_flags(CREATE_NO_WINDOW);
        c
    }

    /// yt-dlp arguments that attach the user's YouTube session.
    pub fn account_args(cfg: &Config) -> Vec<String> {
        match &cfg.account {
            Account::SignedOut => vec![],
            Account::Browser(b) => vec!["--cookies-from-browser".into(), b.clone()],
            Account::CookieFile => vec![
                "--cookies".into(),
                Config::cookie_file().to_string_lossy().into_owned(),
            ],
        }
    }

    fn run(cfg: &Config, args: &[String]) -> Result<String, String> {
        let out = Self::command(cfg)
            .args(["--no-update", "--ignore-config"])
            .args(Self::account_args(cfg))
            .args(args)
            .output()
            .map_err(|e| format!("couldn't start yt-dlp: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let line = err
                .lines()
                .filter(|l| l.starts_with("ERROR"))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(if line.is_empty() {
                err.trim().to_string()
            } else {
                line
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn list(cfg: &Config, args: &[String]) -> Result<Vec<VideoItem>, String> {
        let mut full = vec!["--flat-playlist".to_string(), "-J".to_string()];
        full.extend_from_slice(args);
        let json: Value =
            serde_json::from_str(&Self::run(cfg, &full)?).map_err(|e| e.to_string())?;
        let items = json
            .get("entries")
            .and_then(Value::as_array)
            .map(|e| e.iter().filter_map(VideoItem::from_json).collect())
            .unwrap_or_default();
        Ok(Self::filter(cfg, items))
    }

    pub fn feed(cfg: &Config, feed: Feed) -> Result<Vec<VideoItem>, String> {
        Self::list(
            cfg,
            &[
                "--playlist-end".into(),
                cfg.feed_size.to_string(),
                feed.key().into(),
            ],
        )
    }

    pub fn search(cfg: &Config, q: &str) -> Result<Vec<VideoItem>, String> {
        Self::search_n(cfg, q, 40)
    }

    pub fn search_n(cfg: &Config, q: &str, n: u32) -> Result<Vec<VideoItem>, String> {
        Self::list(cfg, &[format!("ytsearch{n}:{q}")])
    }

    pub fn filter(cfg: &Config, items: Vec<VideoItem>) -> Vec<VideoItem> {
        let kw: Vec<String> = cfg
            .blocked_keywords
            .iter()
            .map(|k| k.to_lowercase())
            .collect();
        items
            .into_iter()
            .filter(|v| !(cfg.hide_shorts && v.short))
            .filter(|v| !(cfg.hide_live && v.live))
            .filter(|v| {
                let t = v.title.to_lowercase();
                !kw.iter().any(|k| t.contains(k))
            })
            .collect()
    }

    /// Reports the view back to YouTube so recommendations keep learning.
    pub fn mark_watched(cfg: &Config, id: &str) {
        if cfg.account == Account::SignedOut || !cfg.send_watch_history {
            return;
        }
        let _ = Self::run(
            cfg,
            &[
                "--mark-watched".into(),
                "--simulate".into(),
                "--quiet".into(),
                format!("https://www.youtube.com/watch?v={id}"),
            ],
        );
    }

    /// yt-dlp format for mpv's ytdl hook (best video up to the preferred
    /// height + best audio).
    pub fn ytdl_format(cfg: &Config) -> String {
        format!("bv*[height<={}]+ba/b", cfg.quality)
    }

    /// `--ytdl-raw-options` that attach the user's YouTube session.
    pub fn ytdl_raw(cfg: &Config) -> Vec<String> {
        match &cfg.account {
            Account::SignedOut => vec![],
            Account::Browser(b) => vec![format!("cookies-from-browser={b}")],
            Account::CookieFile => vec![format!("cookies={}", Config::cookie_file().display())],
        }
    }

    /// Launches mpv (or compatible) pointed at our yt-dlp + session, and
    /// returns how long the viewer watched once the player closes (blocking).
    pub fn play(cfg: &Config, item: &VideoItem) -> Result<Playback, String> {
        let mut c = Command::new(&cfg.player);
        // mpv saves the position it was closed at into a private folder, which
        // is how we learn how far the viewer got (for the For You feed).
        let is_mpv = std::path::Path::new(&cfg.player)
            .file_stem()
            .is_some_and(|s| s.to_string_lossy().to_lowercase().contains("mpv"));
        let later =
            std::env::temp_dir().join(format!("nullyt-watch-{}-{}", std::process::id(), item.id));
        if is_mpv {
            let _ = std::fs::remove_dir_all(&later);
            c.arg("--save-position-on-quit")
                .arg(format!("--watch-later-dir={}", later.display()))
                .arg("--watch-later-options=start");
        }
        c.arg(format!(
            "--script-opts=ytdl_hook-ytdl_path={}",
            Self::exe(cfg).display()
        ))
        .arg(format!("--ytdl-format={}", Self::ytdl_format(cfg)))
        .arg(format!("--title={}", item.title))
        .arg("--force-window=immediate");
        let raw = Self::ytdl_raw(cfg);
        if !raw.is_empty() {
            c.arg(format!("--ytdl-raw-options={}", raw.join(",")));
        }
        let child = c.arg(item.url()).spawn().map_err(|e| {
            format!(
                "couldn't start player \"{}\": {e}. Set the player path in Settings.",
                cfg.player
            )
        })?;
        Ok(Playback {
            child,
            started: std::time::Instant::now(),
            later: is_mpv.then_some(later),
            duration: item.duration,
        })
    }
}

/// A running player.
pub struct Playback {
    child: std::process::Child,
    started: std::time::Instant,
    later: Option<PathBuf>,
    duration: Option<f64>,
}

impl Playback {
    /// Waits for the player to close and estimates how much was watched: the
    /// position mpv saved on quit, or (if it played to the end and saved
    /// nothing) the time it was open, capped at the video's length.
    pub fn wait(mut self) -> std::time::Duration {
        let _ = self.child.wait();
        let open = self.started.elapsed().as_secs_f64();
        let saved = self.later.as_ref().and_then(|dir| {
            let pos = std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .find_map(|f| saved_position(&std::fs::read_to_string(f.path()).ok()?));
            let _ = std::fs::remove_dir_all(dir);
            pos
        });
        let secs = saved.unwrap_or_else(|| match self.duration {
            // No saved position: either it finished, or (live / unknown length) wall time.
            Some(len) if open >= len * 0.8 => len,
            Some(len) => open.min(len),
            None => open,
        });
        std::time::Duration::from_secs_f64(secs.max(0.0))
    }
}

/// `start=123.45` from an mpv watch-later file.
pub fn saved_position(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix("start=")?.trim().parse().ok())
}

pub fn fmt_count(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}K", n as f64 / 1e3),
        n => n.to_string(),
    }
}

pub fn fmt_duration(s: f64) -> String {
    let s = s as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mpv_saved_position() {
        let file = "# https://www.youtube.com/watch?v=abc
start=123.456000
";
        assert_eq!(saved_position(file), Some(123.456));
        assert_eq!(
            saved_position(
                "volume=50
"
            ),
            None
        );
    }
}
