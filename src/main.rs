#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod foryou;
mod player;
mod ytdlp;

use config::{Account, Config};
use foryou::{ForYou, TOPICS};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The built-in player, created on first use.
    player: Mutex<Option<player::Player>>,
    now: Mutex<Option<NowPlaying>>,
    /// A frame is on its way to the UI; skip new ones until it lands.
    frame_pending: Arc<AtomicBool>,
    /// Last mouse/keyboard activity over the player (controls auto-hide).
    last_activity: Mutex<std::time::Instant>,
    sleep: Mutex<Option<Sleep>>,
}

#[derive(Clone, Copy)]
enum Sleep {
    At(std::time::Instant),
    EndOfVideo,
}

/// What the built-in player is showing.
struct NowPlaying {
    list: Vec<VideoItem>,
    index: usize,
    /// Furthest point reached, for the For You feed.
    furthest: f64,
    /// Page to go back to.
    back_to: i32,
    /// Chapter marks were read for this video.
    chapters_read: bool,
}

impl NowPlaying {
    fn item(&self) -> &VideoItem {
        &self.list[self.index]
    }
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
    ui.set_player_quality_index(ui.get_quality_index());
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
    ui.set_embedded_player(c.embedded_player);
    ui.set_player_volume((c.volume / 100.0) as f32);
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
    c.embedded_player = ui.get_embedded_player();
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

/// Plays `list[index]` inside the app. Errors if libmpv can't start.
fn play_embedded(
    ui: &AppWindow,
    state: &Arc<State>,
    list: Vec<VideoItem>,
    index: usize,
) -> Result<(), String> {
    let cfg = state.cfg();
    let mut slot = state.player.lock().unwrap();
    if slot.is_none() {
        let (weak, pending) = (ui.as_weak(), state.frame_pending.clone());
        let p = player::Player::new(
            player::Options {
                ytdl_path: Engine::exe(&cfg).display().to_string(),
                ytdl_format: Engine::ytdl_format(&cfg),
                ytdl_raw: Engine::ytdl_raw(&cfg),
                volume: cfg.volume,
                max_size: (1920, 1080),
            },
            move |frame| {
                // The UI is still drawing the last frame: drop this one.
                if pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                let pending = pending.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_player_frame(Image::from_rgba8(frame));
                    pending.store(false, Ordering::Release);
                });
            },
        )?;
        *slot = Some(p);
    }
    let item = list[index].clone();
    slot.as_ref().unwrap().load(&item.url())?;
    drop(slot);

    let back_to = match state.now.lock().unwrap().take() {
        Some(prev) => prev.back_to,
        None => ui.get_page(),
    };
    ui.set_player_frame(Image::default());
    ui.set_player_title(item.title.clone().into());
    ui.set_player_channel(item.channel.clone().into());
    ui.set_player_progress(0.0);
    ui.set_player_time("".into());
    ui.set_player_status("Loading…".into());
    ui.set_player_paused(false);
    ui.set_player_has_next(index + 1 < list.len());
    ui.set_page(5);
    // "Up next" card: the thumbnail is already loaded if it's on screen.
    if let Some(next) = list.get(index + 1) {
        ui.set_player_next_title(next.title.clone().into());
        let thumb = ui
            .get_videos()
            .iter()
            .find(|v| v.id.as_str() == next.id)
            .map(|v| v.thumb)
            .unwrap_or_default();
        ui.set_player_next_thumb(thumb);
    }
    ui.set_player_chapter("".into());
    ui.set_player_chapters(ModelRc::new(VecModel::<f32>::default()));
    ui.set_player_controls(true);
    *state.last_activity.lock().unwrap() = std::time::Instant::now();
    *state.now.lock().unwrap() = Some(NowPlaying {
        list,
        index,
        furthest: 0.0,
        back_to,
        chapters_read: false,
    });
    let id = item.id.clone();
    std::thread::spawn(move || Engine::mark_watched(&cfg, &id));
    Ok(())
}

/// Stops the current video and tells the For You feed how much was watched.
fn finish_current(state: &Arc<State>) -> Option<NowPlaying> {
    let now = state.now.lock().unwrap().take()?;
    if let Some(p) = state.player.lock().unwrap().as_ref() {
        p.stop();
    }
    state.fy.lock().unwrap().watched(
        now.item(),
        std::time::Duration::from_secs_f64(now.furthest.max(0.0)),
    );
    Some(now)
}

fn close_player(ui: &AppWindow, state: &Arc<State>) {
    let back = finish_current(state).map(|n| n.back_to).unwrap_or(0);
    if ui.get_fullscreen() {
        ui.window().set_fullscreen(false);
        ui.set_fullscreen(false);
    }
    ui.set_player_frame(Image::default());
    ui.set_page(back);
    sync_foryou(ui, &state.fy.lock().unwrap());
    // Home learns from what was just watched.
    if back == 0 && session_home(&state.cfg()) {
        load(ui, state, 0, None);
    }
}

fn play_next(ui: &AppWindow, state: &Arc<State>) {
    let Some(now) = finish_current(state) else {
        return;
    };
    let next = now.index + 1;
    if next >= now.list.len() {
        *state.now.lock().unwrap() = Some(now);
        close_player(ui, state);
        return;
    }
    play_index(ui, state, now, next);
}

/// Switches the player to `list[index]`, keeping where "back" goes.
fn play_index(ui: &AppWindow, state: &Arc<State>, now: NowPlaying, index: usize) {
    let list = now.list.clone();
    *state.now.lock().unwrap() = Some(NowPlaying {
        furthest: 0.0,
        chapters_read: false,
        ..now
    });
    if let Err(e) = play_embedded(ui, state, list, index) {
        ui.set_player_status(e.into());
    }
}

/// Previous: restart the video, or (near its start) go to the one before.
fn play_prev(ui: &AppWindow, state: &Arc<State>) {
    let pos = state
        .player
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|p| p.double("time-pos"))
        .unwrap_or(0.0);
    let index = state
        .now
        .lock()
        .unwrap()
        .as_ref()
        .map(|n| n.index)
        .unwrap_or(0);
    if pos > 3.0 || index == 0 {
        if let Some(p) = state.player.lock().unwrap().as_ref() {
            p.seek(0.0);
        }
        return;
    }
    if let Some(now) = finish_current(state) {
        play_index(ui, state, now, index - 1);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let (tx_placeholder, _) = mpsc::channel();
    let cfg = Config::load();
    let state = Arc::new(State {
        fy: Mutex::new(ForYou::load(cfg.foryou_remember)),
        player: Mutex::new(None),
        now: Mutex::new(None),
        frame_pending: Arc::new(AtomicBool::new(false)),
        last_activity: Mutex::new(std::time::Instant::now()),
        sleep: Mutex::new(None),
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
            let list = state.items.lock().unwrap().clone();
            let Some(item) = list.get(i as usize).cloned() else {
                return;
            };
            let cfg = state.cfg();
            if cfg.embedded_player && player::available() {
                let ui = weak.unwrap();
                match play_embedded(&ui, &state, list, i as usize) {
                    Ok(()) => return,
                    // libmpv failed to start: fall through to the external player.
                    Err(e) => ui.set_status(format!("Built-in player: {e}").into()),
                }
            }
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

    // ── Built-in player ───────────────────────────────────────────────────
    ui.on_player_toggle({
        let state = state.clone();
        move || {
            if let Some(p) = state.player.lock().unwrap().as_ref() {
                p.set_flag("pause", !p.flag("pause"));
            }
        }
    });
    ui.on_player_seek({
        let state = state.clone();
        move |f| {
            if let Some(p) = state.player.lock().unwrap().as_ref()
                && let Some(d) = p.double("duration")
            {
                p.seek(d * f as f64);
            }
        }
    });
    ui.on_player_skip({
        let state = state.clone();
        move |d| {
            if let Some(p) = state.player.lock().unwrap().as_ref() {
                p.seek_by(d as f64);
            }
        }
    });
    ui.on_player_set_volume({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |v| {
            let v = v.clamp(0.0, 1.3);
            weak.unwrap().set_player_volume(v);
            if let Some(p) = state.player.lock().unwrap().as_ref() {
                p.set_double("volume", v as f64 * 100.0);
            }
            let mut cfg = state.cfg.lock().unwrap();
            cfg.volume = v as f64 * 100.0;
            cfg.save();
        }
    });
    ui.on_player_close({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || close_player(&weak.unwrap(), &state)
    });
    ui.on_player_next({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || play_next(&weak.unwrap(), &state)
    });
    ui.on_toggle_fullscreen({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let on = !ui.get_fullscreen();
            ui.window().set_fullscreen(on);
            ui.set_fullscreen(on);
        }
    });
    ui.on_player_activity({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            *state.last_activity.lock().unwrap() = std::time::Instant::now();
            weak.unwrap().set_player_controls(true);
        }
    });
    ui.on_player_prev({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || play_prev(&weak.unwrap(), &state)
    });
    ui.on_player_toggle_captions({
        let (weak, state) = (ui.as_weak(), state.clone());
        move || {
            let ui = weak.unwrap();
            let on = !ui.get_player_captions();
            ui.set_player_captions(on);
            if let Some(p) = state.player.lock().unwrap().as_ref() {
                p.set_string("sid", if on { "auto" } else { "no" });
                if on && p.double("sid").is_none() && p.string("sid").as_deref() == Some("no") {
                    ui.set_player_status("No captions for this video".into());
                }
            }
        }
    });
    ui.on_player_set_speed({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |sp| {
            weak.unwrap().set_player_speed(sp);
            if let Some(p) = state.player.lock().unwrap().as_ref() {
                p.set_double("speed", sp as f64);
            }
        }
    });
    ui.on_player_set_quality({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |i| {
            let ui = weak.unwrap();
            let i = i.clamp(0, 7);
            ui.set_player_quality_index(i);
            ui.set_quality_index(i);
            let cfg = {
                let mut cfg = state.cfg.lock().unwrap();
                cfg.quality = QUALITIES[i as usize];
                cfg.save();
                cfg.clone()
            };
            let url = state.now.lock().unwrap().as_ref().map(|n| n.item().url());
            if let (Some(p), Some(url)) = (state.player.lock().unwrap().as_ref(), url) {
                let pos = p.double("time-pos").unwrap_or(0.0);
                p.set_string("ytdl-format", &Engine::ytdl_format(&cfg));
                if let Err(e) = p.load_at(&url, pos) {
                    ui.set_player_status(e.into());
                } else {
                    ui.set_player_status("Switching quality…".into());
                }
            }
        }
    });
    ui.on_player_set_sleep({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |m| {
            let ui = weak.unwrap();
            let (sleep, label) = match m {
                0 => (None, "Off".to_string()),
                -1 => (Some(Sleep::EndOfVideo), "End of video".to_string()),
                m => (
                    Some(Sleep::At(
                        std::time::Instant::now() + std::time::Duration::from_secs(m as u64 * 60),
                    )),
                    format!("{m} min"),
                ),
            };
            *state.sleep.lock().unwrap() = sleep;
            ui.set_player_sleep(label.into());
        }
    });

    // Poll playback state for the controls, progress and autoplay.
    let ticker = slint::Timer::default();
    ticker.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        {
            let (weak, state) = (ui.as_weak(), state.clone());
            move || {
                let Some(ui) = weak.upgrade() else { return };
                if ui.get_page() != 5 {
                    return;
                }
                let (pos, dur, paused, eof, err) = {
                    let guard = state.player.lock().unwrap();
                    let Some(p) = guard.as_ref() else { return };
                    (
                        p.double("time-pos"),
                        p.double("duration"),
                        p.flag("pause"),
                        p.flag("eof-reached"),
                        p.error(),
                    )
                };
                if let Some(n) = state.now.lock().unwrap().as_mut()
                    && let Some(t) = pos
                {
                    n.furthest = n.furthest.max(t);
                }
                ui.set_player_paused(paused);
                match (pos, dur) {
                    (Some(t), Some(d)) if d > 0.0 => {
                        ui.set_player_progress((t / d).clamp(0.0, 1.0) as f32);
                        ui.set_player_time(
                            format!("{} / {}", player::fmt_time(t), player::fmt_time(d)).into(),
                        );
                        ui.set_player_status("".into());
                    }
                    (Some(t), _) => {
                        ui.set_player_time(player::fmt_time(t).into());
                        ui.set_player_status("".into());
                    }
                    _ => {}
                }
                if let Some(e) = err {
                    ui.set_player_status(format!("Couldn't play this video: {e}").into());
                }

                // Chapters: marks once per video, the current title every tick.
                if let Some(d) = dur.filter(|d| *d > 0.0) {
                    let guard = state.player.lock().unwrap();
                    if let Some(p) = guard.as_ref() {
                        ui.set_player_chapter(p.chapter_title().unwrap_or_default().into());
                        let mut now = state.now.lock().unwrap();
                        if let Some(n) = now.as_mut()
                            && !n.chapters_read
                        {
                            n.chapters_read = true;
                            let marks: Vec<f32> =
                                p.chapters().iter().map(|t| (t / d) as f32).collect();
                            ui.set_player_chapters(ModelRc::new(Rc::new(VecModel::from(marks))));
                        }
                    }
                }

                // Controls hide after 3 s without input while playing.
                let idle = state.last_activity.lock().unwrap().elapsed();
                ui.set_player_controls(paused || ui.get_sheet() != 0 || idle.as_secs_f32() < 3.0);

                // Sleep timer.
                let sleep = *state.sleep.lock().unwrap();
                if let Some(Sleep::At(t)) = sleep
                    && std::time::Instant::now() >= t
                {
                    if let Some(p) = state.player.lock().unwrap().as_ref() {
                        p.set_flag("pause", true);
                    }
                    *state.sleep.lock().unwrap() = None;
                    ui.set_player_sleep("Off".into());
                }
                if eof {
                    if let Some(Sleep::EndOfVideo) = sleep {
                        *state.sleep.lock().unwrap() = None;
                        ui.set_player_sleep("Off".into());
                    } else if state.cfg().autoplay {
                        play_next(&ui, &state);
                    }
                }
            }
        },
    );

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
