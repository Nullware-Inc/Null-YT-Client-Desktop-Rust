#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod foryou;
mod ytdlp;

use config::{Account, Config};
use foryou::{ForYou, TOPICS};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use ytdlp::{Engine, Feed, VideoItem, fmt_count, fmt_duration};

slint::include_modules!();

const QUALITIES: [u32; 8] = [144, 240, 360, 480, 720, 1080, 1440, 2160];
const EFFECTS: [&str; 3] = ["none", "mica", "acrylic"];

struct State {
    cfg: Mutex<Config>,
    /// Taste for the session "For you" feed.
    fy: Mutex<ForYou>,
    items: Mutex<Vec<VideoItem>>,
    /// Bumped on every feed load so late thumbnails for an old feed are dropped.
    generation: AtomicU64,
    thumbs: Mutex<mpsc::Sender<(u64, usize, String)>>,
}

impl State {
    fn cfg(&self) -> Config {
        self.cfg.lock().unwrap().clone()
    }
}

/// Home shows Null YT's own session mix (always when signed out).
fn session_home(c: &Config) -> bool {
    c.account == Account::SignedOut || c.home_source == "session"
}

/// Mirrors the For You state into the UI: welcome screen, topic chips,
/// signal count.
fn sync_foryou(ui: &AppWindow, fy: &ForYou) {
    ui.set_show_welcome(!fy.has_taste());
    ui.set_signal_count(fy.signals.len() as i32);
    let picked = fy.interests();
    let topics: Vec<Topic> = TOPICS
        .iter()
        .map(|t| Topic {
            name: (*t).into(),
            picked: picked.iter().any(|p| p == t),
        })
        .collect();
    ui.set_topics(ModelRc::new(Rc::new(VecModel::from(topics))));
}

fn hex_to_color(s: &str) -> Color {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0xff3d5a);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn color_to_hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}

fn account_label(a: &Account) -> String {
    match a {
        Account::SignedOut => "Not signed in".into(),
        Account::Browser(b) => format!("Synced via your {b} session"),
        Account::CookieFile => "Synced via imported cookies.txt".into(),
    }
}

fn push_config(ui: &AppWindow, c: &Config) {
    let t = ui.global::<Theme>();
    t.set_glass(c.glass);
    t.set_dark(c.dark);
    t.set_aurora(c.aurora);
    t.set_accent(hex_to_color(&c.accent));
    t.set_radius(c.radius);
    t.set_tint(c.tint);
    t.set_rim(c.rim);
    ui.set_columns(c.columns);
    ui.set_hide_shorts(c.hide_shorts);
    ui.set_hide_live(c.hide_live);
    ui.set_send_history(c.send_watch_history);
    ui.set_quality_index(QUALITIES.iter().position(|q| *q == c.quality).unwrap_or(5) as i32);
    ui.set_player(c.player.clone().into());
    ui.set_effect_index(
        EFFECTS
            .iter()
            .position(|e| *e == c.window_effect)
            .unwrap_or(0) as i32,
    );
    ui.set_blocked(c.blocked_keywords.join(", ").into());
    ui.set_account_label(account_label(&c.account).into());
    ui.set_signed_in(c.account != Account::SignedOut);
    ui.set_session_home(session_home(c));
    ui.set_foryou_remember(c.foryou_remember);
}

fn pull_config(ui: &AppWindow, c: &mut Config) {
    let t = ui.global::<Theme>();
    c.glass = t.get_glass();
    c.dark = t.get_dark();
    c.aurora = t.get_aurora();
    c.accent = color_to_hex(t.get_accent());
    c.radius = t.get_radius();
    c.tint = t.get_tint();
    c.rim = t.get_rim();
    c.columns = ui.get_columns();
    c.hide_shorts = ui.get_hide_shorts();
    c.hide_live = ui.get_hide_live();
    c.send_watch_history = ui.get_send_history();
    c.foryou_remember = ui.get_foryou_remember();
    c.quality = QUALITIES[ui.get_quality_index().clamp(0, 7) as usize];
    c.player = ui.get_player().to_string();
    c.window_effect = EFFECTS[ui.get_effect_index().clamp(0, 2) as usize].into();
    c.blocked_keywords = ui
        .get_blocked()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
}

/// Native window material (Windows 11 Mica / Acrylic) behind the glass UI.
fn apply_window_effect(ui: &AppWindow, c: &Config) {
    #[cfg(target_os = "windows")]
    {
        let handle = ui.window().window_handle();
        let _ = window_vibrancy::clear_mica(&handle);
        let _ = window_vibrancy::clear_acrylic(&handle);
        let ok = match c.window_effect.as_str() {
            "mica" => window_vibrancy::apply_mica(&handle, Some(c.dark)).is_ok(),
            "acrylic" => window_vibrancy::apply_acrylic(&handle, Some((16, 16, 22, 110))).is_ok(),
            _ => false,
        };
        ui.global::<Theme>().set_translucent(ok);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (ui, c);
}

fn to_row(v: &VideoItem) -> Video {
    let mut meta = vec![];
    if let Some(n) = v.views {
        meta.push(format!(
            "{} {}",
            fmt_count(n),
            if v.live { "watching" } else { "views" }
        ));
    }
    Video {
        id: v.id.clone().into(),
        title: v.title.clone().into(),
        channel: v.channel.clone().into(),
        meta: meta.join("  ·  ").into(),
        duration: if v.short {
            "SHORT".into()
        } else {
            v.duration.map(fmt_duration).unwrap_or_default().into()
        },
        live: v.live,
        thumb: Image::default(),
    }
}

/// Background workers that download + decode thumbnails off the UI thread.
fn spawn_thumbnail_workers(
    ui: slint::Weak<AppWindow>,
    state: Arc<State>,
) -> mpsc::Sender<(u64, usize, String)> {
    let (tx, rx) = mpsc::channel::<(u64, usize, String)>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..6 {
        let (rx, ui, state) = (rx.clone(), ui.clone(), state.clone());
        std::thread::spawn(move || {
            loop {
                let Ok((generation, index, url)) = rx.lock().unwrap().recv() else {
                    return;
                };
                if state.generation.load(Ordering::Relaxed) != generation {
                    continue;
                }
                let Some(buf) = fetch_thumb(&url) else {
                    continue;
                };
                let state = state.clone();
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    if state.generation.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    let model = ui.get_videos();
                    if let Some(mut row) = model.row_data(index) {
                        row.thumb = Image::from_rgba8(buf);
                        model.set_row_data(index, row);
                    }
                });
            }
        });
    }
    tx
}

fn fetch_thumb(url: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut res = ureq::get(url).call().ok()?;
    let bytes = res.body_mut().read_to_vec().ok()?;
    let img = image::load_from_memory(&bytes)
        .ok()?
        .thumbnail(640, 360)
        .to_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(
        img.as_raw(),
        img.width(),
        img.height(),
    ))
}

fn load(ui: &AppWindow, state: &Arc<State>, page: i32, query: Option<String>) {
    let generation = state.generation.fetch_add(1, Ordering::Relaxed) + 1;
    ui.set_loading(true);
    ui.set_status("".into());
    ui.set_videos(ModelRc::new(VecModel::<Video>::default()));
    let cfg = state.cfg();
    let session = page == 0 && session_home(&cfg);
    ui.set_session_home(session_home(&cfg));
    if session && !state.fy.lock().unwrap().has_taste() {
        // Nothing to go on yet: the topic picker takes Home's place.
        ui.set_loading(false);
        ui.set_show_welcome(true);
        return;
    }
    ui.set_show_welcome(false);
    let fy = session.then(|| state.fy.lock().unwrap().clone());
    let (weak, state) = (ui.as_weak(), state.clone());
    std::thread::spawn(move || {
        let result = match (page, query) {
            (0, _) if fy.is_some() => {
                let (items, visitor) = fy
                    .unwrap()
                    .build(|q| Engine::search_n(&cfg, q, 20), cfg.feed_size as usize);
                let items = Engine::filter(&cfg, items);
                let mut f = state.fy.lock().unwrap();
                f.visitor = visitor;
                f.served(items.iter().map(|v| v.id.clone()).collect());
                Ok(items)
            }
            (2, Some(q)) if !q.trim().is_empty() => Engine::search(&cfg, &q),
            (2, _) => Ok(vec![]),
            _ if cfg.account == Account::SignedOut => Err(
                "Connect your YouTube account in Settings → Account to load your feed. Search works signed out."
                    .into(),
            ),
            (1, _) => Engine::feed(&cfg, Feed::Subscriptions),
            (3, _) => Engine::feed(&cfg, Feed::History),
            _ => Engine::feed(&cfg, Feed::Recommended),
        };
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if state.generation.load(Ordering::Relaxed) != generation {
                return;
            }
            ui.set_loading(false);
            match result {
                Ok(items) => {
                    let rows: Vec<Video> = items.iter().map(to_row).collect();
                    ui.set_videos(ModelRc::new(Rc::new(VecModel::from(rows))));
                    let tx = state.thumbs.lock().unwrap().clone();
                    for (i, v) in items.iter().enumerate() {
                        let _ = tx.send((generation, i, v.thumbnail.clone()));
                    }
                    if items.is_empty() && page != 2 {
                        ui.set_status(
                            if session {
                                "Nothing to suggest yet. Watch a video, search, or pick some topics."
                            } else {
                                "YouTube returned an empty feed. Watch a few videos and refresh."
                            }
                            .into(),
                        );
                    }
                    *state.items.lock().unwrap() = items;
                }
                Err(e) => ui.set_status(e.into()),
            }
        });
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let (tx_placeholder, _) = mpsc::channel();
    let cfg = Config::load();
    let state = Arc::new(State {
        fy: Mutex::new(ForYou::load(cfg.foryou_remember)),
        cfg: Mutex::new(cfg),
        items: Mutex::new(vec![]),
        generation: AtomicU64::new(0),
        thumbs: Mutex::new(tx_placeholder),
    });
    *state.thumbs.lock().unwrap() = spawn_thumbnail_workers(ui.as_weak(), state.clone());
    push_config(&ui, &state.cfg());
    sync_foryou(&ui, &state.fy.lock().unwrap());

    let query = Rc::new(std::cell::RefCell::new(String::new()));

    ui.on_open_page({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |p| {
            let ui = weak.unwrap();
            ui.set_page(p);
            if p != 4 {
                load(&ui, &state, p, None);
            }
        }
    });
    ui.on_search({
        let (weak, state, query) = (ui.as_weak(), state.clone(), query.clone());
        move |q| {
            *query.borrow_mut() = q.to_string();
            state.fy.lock().unwrap().searched(&q);
            sync_foryou(&weak.unwrap(), &state.fy.lock().unwrap());
            load(&weak.unwrap(), &state, 2, Some(q.to_string()));
        }
    });
    ui.on_refresh({
        let (weak, state, query) = (ui.as_weak(), state.clone(), query.clone());
        move || {
            let ui = weak.unwrap();
            load(&ui, &state, ui.get_page(), Some(query.borrow().clone()));
        }
    });
    ui.on_play({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |i| {
            let Some(item) = state.items.lock().unwrap().get(i as usize).cloned() else {
                return;
            };
            let cfg = state.cfg();
            match Engine::play(&cfg, &item) {
                Ok(playback) => {
                    let (cfg2, id) = (cfg.clone(), item.id.clone());
                    std::thread::spawn(move || Engine::mark_watched(&cfg2, &id));
                    // When the player closes, learn from how much was watched,
                    // and refresh Home if that's where the viewer is.
                    let (weak, state) = (weak.clone(), state.clone());
                    std::thread::spawn(move || {
                        let watched = playback.wait();
                        state.fy.lock().unwrap().watched(&item, watched);
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            sync_foryou(&ui, &state.fy.lock().unwrap());
                            if ui.get_page() == 0 && session_home(&state.cfg()) {
                                load(&ui, &state, 0, None);
                            }
                        });
                    });
                }
                Err(e) => weak.unwrap().set_status(e.into()),
            }
        }
    });
    ui.on_settings_changed({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let ui = weak.unwrap();
            let mut cfg = state.cfg.lock().unwrap();
            let before = cfg.window_effect.clone();
            let remember = cfg.foryou_remember;
            pull_config(&ui, &mut cfg);
            cfg.save();
            if before != cfg.window_effect {
                apply_window_effect(&ui, &cfg);
            }
            if remember != cfg.foryou_remember {
                state.fy.lock().unwrap().set_remember(cfg.foryou_remember);
            }
        }
    });
    let set_account = {
        let (weak, state) = (ui.as_weak(), state.clone());
        move |a: Account| {
            let ui = weak.unwrap();
            let mut cfg = state.cfg.lock().unwrap();
            cfg.account = a;
            cfg.save();
            ui.set_account_label(account_label(&cfg.account).into());
            ui.set_signed_in(cfg.account != Account::SignedOut);
        }
    };
    ui.on_use_browser({
        let set = set_account.clone();
        move |b| set(Account::Browser(b.to_string()))
    });
    ui.on_import_cookies({
        let set = set_account.clone();
        move || {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("cookies.txt", &["txt"])
                .pick_file()
                && std::fs::copy(p, Config::cookie_file()).is_ok()
            {
                set(Account::CookieFile);
            }
        }
    });
    ui.on_sign_out({
        let set = set_account.clone();
        move || {
            let _ = std::fs::remove_file(Config::cookie_file());
            set(Account::SignedOut)
        }
    });

    // ── Session "For you" feed ────────────────────────────────────────────
    ui.on_toggle_topic({
        let weak = ui.as_weak();
        move |i| {
            let model = weak.unwrap().get_topics();
            if let Some(mut t) = model.row_data(i as usize) {
                t.picked = !t.picked;
                model.set_row_data(i as usize, t);
            }
        }
    });
    ui.on_save_topics({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let ui = weak.unwrap();
            let picked: Vec<String> = ui
                .get_topics()
                .iter()
                .filter(|t| t.picked)
                .map(|t| t.name.to_string())
                .collect();
            state.fy.lock().unwrap().set_interests(&picked);
            ui.set_tune_open(false);
            sync_foryou(&ui, &state.fy.lock().unwrap());
            load(&ui, &state, 0, None);
        }
    });
    ui.on_open_tune({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let ui = weak.unwrap();
            sync_foryou(&ui, &state.fy.lock().unwrap());
            ui.set_tune_open(true);
        }
    });
    ui.on_cancel_tune({
        let weak = ui.as_weak();
        move || weak.unwrap().set_tune_open(false)
    });
    ui.on_start_fresh({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let ui = weak.unwrap();
            state.fy.lock().unwrap().reset();
            ui.set_tune_open(false);
            sync_foryou(&ui, &state.fy.lock().unwrap());
            if ui.get_page() == 0 {
                load(&ui, &state, 0, None);
            }
        }
    });
    ui.on_not_interested({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |i| {
            let Some(item) = state.items.lock().unwrap().get(i as usize).cloned() else {
                return;
            };
            state.fy.lock().unwrap().not_interested(&item);
            let ui = weak.unwrap();
            sync_foryou(&ui, &state.fy.lock().unwrap());
            load(&ui, &state, 0, None);
        }
    });
    ui.on_set_home_session({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |on| {
            let ui = weak.unwrap();
            {
                let mut cfg = state.cfg.lock().unwrap();
                cfg.home_source = if on { "session" } else { "youtube" }.into();
                cfg.save();
            }
            load(&ui, &state, 0, None);
        }
    });

    ui.show()?;
    apply_window_effect(&ui, &state.cfg());

    // First run: fetch yt-dlp, then load the home feed and update in background.
    {
        let (weak, state) = (ui.as_weak(), state.clone());
        ui.set_loading(true);
        ui.set_status("Starting engine…".into());
        std::thread::spawn(move || {
            let cfg = state.cfg();
            let res = Engine::ensure_installed(&cfg);
            let st = state.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| match res {
                Ok(()) => load(&ui, &st, 0, None),
                Err(e) => {
                    ui.set_loading(false);
                    ui.set_status(e.into());
                }
            });
            Engine::update(&cfg);
        });
    }

    slint::run_event_loop()
}
