//! "For you" without an account: a recommendation feed built from what you
//! do in Null YT, mostly in this session. Same algorithm as the Dart core
//! (`lib/src/discover/for_you.dart`).
//!
//! Every video you watch becomes a seed. We pull YouTube's own related list
//! for the strongest recent seeds (signed out, one persistent visitor id so
//! YouTube treats it as one viewing session) and score each candidate by how
//! many seeds point at it, how high, and how recent and engaged those seeds
//! were. Channels and title words you keep coming back to get a lift; skips,
//! "Not interested" and videos shown on Home but never opened push things
//! down. A last pass spreads channels out and mixes in picks from your other
//! recent interests so the feed doesn't tunnel.

use crate::config::data_dir;
use crate::ytdlp::VideoItem;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TOPICS: [&str; 18] = [
    "Music",
    "Gaming",
    "Tech",
    "Science",
    "Comedy",
    "News",
    "Sports",
    "Cooking",
    "Movies",
    "Anime",
    "Cars",
    "Fitness",
    "Podcasts",
    "DIY",
    "Education",
    "Travel",
    "Art",
    "History",
];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SignalKind {
    Watch,
    Skip,
    NotInterested,
    Search,
    Interest,
}

/// One thing the viewer did that tells us about their taste.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub kind: SignalKind,
    /// ~0.3 (barely watched) to 2 (watched to the end); negative for skips
    /// and "Not interested".
    pub strength: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Unix seconds.
    pub at: u64,
}

/// A weighted list of candidates: the related list of one seed video, or the
/// results of one search.
pub struct Source {
    pub weight: f64,
    pub items: Vec<VideoItem>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ForYou {
    pub signals: Vec<Signal>,
    pub hidden: HashSet<String>,
    pub impressions: HashMap<String, u32>,
    pub visitor: Option<String>,
    #[serde(skip)]
    last_served: Vec<String>,
    #[serde(skip)]
    pub remember: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path() -> std::path::PathBuf {
    data_dir().join("foryou.json")
}

impl ForYou {
    pub fn load(remember: bool) -> Self {
        let mut me: Self = if remember {
            std::fs::read_to_string(path())
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        };
        me.remember = remember;
        me
    }

    pub fn save(&mut self) {
        if !self.remember {
            return;
        }
        // The newest signals matter most anyway.
        if self.signals.len() > 400 {
            let extra = self.signals.len() - 400;
            self.signals.drain(..extra);
        }
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(path(), s);
        }
    }

    pub fn set_remember(&mut self, on: bool) {
        self.remember = on;
        if on {
            self.save();
        } else {
            let _ = std::fs::remove_file(path());
        }
    }

    pub fn reset(&mut self) {
        let remember = self.remember;
        *self = Self {
            remember,
            ..Self::default()
        };
        let _ = std::fs::remove_file(path());
    }

    /// Enough to go on: something watched, searched or picked.
    pub fn has_taste(&self) -> bool {
        self.signals.iter().any(|s| s.strength > 0.0)
    }

    pub fn interests(&self) -> Vec<String> {
        self.signals
            .iter()
            .filter(|s| s.kind == SignalKind::Interest)
            .filter_map(|s| s.query.clone())
            .collect()
    }

    fn push(
        &mut self,
        kind: SignalKind,
        strength: f64,
        video: Option<VideoItem>,
        query: Option<String>,
    ) {
        self.signals.push(Signal {
            kind,
            strength,
            video,
            query,
            at: now(),
        });
        self.save();
    }

    /// The viewer stopped watching. Under 20 s and under a quarter of the
    /// video counts as a skip; otherwise the further they got, the stronger.
    pub fn watched(&mut self, v: &VideoItem, watched: Duration) {
        let secs = watched.as_secs_f64();
        let fraction = match v.duration {
            Some(len) if len > 0.0 => (secs / len).clamp(0.0, 1.0),
            _ => (secs / 300.0).clamp(0.0, 1.0),
        };
        self.impressions.remove(&v.id);
        if secs < 20.0 && fraction < 0.25 {
            self.push(SignalKind::Skip, -0.6, Some(v.clone()), None);
        } else {
            let bonus = if secs >= 600.0 { 0.4 } else { 0.0 };
            let s = (0.4 + 1.6 * fraction + bonus).clamp(0.3, 2.0);
            self.push(SignalKind::Watch, s, Some(v.clone()), None);
        }
    }

    pub fn searched(&mut self, q: &str) {
        let q = q.trim();
        if q.is_empty() || q.contains("youtube.com/") || q.contains("youtu.be/") {
            return;
        }
        self.push(SignalKind::Search, 0.8, None, Some(q.to_string()));
    }

    pub fn not_interested(&mut self, v: &VideoItem) {
        self.hidden.insert(v.id.clone());
        self.push(SignalKind::NotInterested, -1.5, Some(v.clone()), None);
    }

    pub fn set_interests(&mut self, picked: &[String]) {
        self.signals.retain(|s| s.kind != SignalKind::Interest);
        let t = now();
        for p in picked {
            self.signals.push(Signal {
                kind: SignalKind::Interest,
                strength: 0.7,
                video: None,
                query: Some(p.clone()),
                at: t,
            });
        }
        self.save();
    }

    /// Records what Home just showed; the top ones that weren't opened by the
    /// next build count as passed over.
    pub fn served(&mut self, ids: Vec<String>) {
        for id in self.last_served.iter().take(16) {
            *self.impressions.entry(id.clone()).or_default() += 1;
        }
        self.last_served = ids;
        self.save();
    }

    /// Builds the feed. Network-heavy: call it off the UI thread on a clone.
    pub fn build(
        &self,
        search: impl Fn(&str) -> Result<Vec<VideoItem>, String> + Sync,
        size: usize,
    ) -> (Vec<VideoItem>, Option<String>) {
        let t = now();
        let mut video_seeds: Vec<(f64, VideoItem)> = vec![];
        let mut query_seeds: Vec<(f64, String)> = vec![];
        let mut channel_aff: HashMap<String, f64> = HashMap::new();
        let mut terms: HashMap<String, f64> = HashMap::new();

        for (i, s) in self.signals.iter().rev().enumerate() {
            // Each newer action halves the pull of older ones every ~4 steps;
            // anything from a previous session (6 h+) counts at most a third.
            let old = if t.saturating_sub(s.at) >= 6 * 3600 {
                0.35
            } else {
                1.0
            };
            let w = s.strength * 0.84f64.powi(i as i32).max(0.04) * old;
            if let Some(v) = &s.video {
                *channel_aff.entry(v.channel_key()).or_default() += w;
                for tok in tokens(&v.title) {
                    *terms.entry(tok).or_default() += w;
                }
                if s.kind == SignalKind::Watch && !video_seeds.iter().any(|(_, x)| x.id == v.id) {
                    video_seeds.push((w, v.clone()));
                }
            } else if let Some(q) = &s.query
                && s.strength > 0.0
                && !query_seeds.iter().any(|(_, x)| x == q)
            {
                query_seeds.push((w, q.clone()));
                for tok in tokens(q) {
                    *terms.entry(tok).or_default() += w * 0.5;
                }
            }
        }
        video_seeds.sort_by(|a, b| b.0.total_cmp(&a.0));
        query_seeds.sort_by(|a, b| b.0.total_cmp(&a.0));

        // Searches and topics matter most when there's little watch history.
        let query_count = match video_seeds.len() {
            0 => 4,
            n if n >= 6 => 1,
            _ => 2,
        };
        let client = Related {
            visitor: std::sync::Mutex::new(self.visitor.clone()),
        };
        // The first request sets up the visitor id the others then share.
        let seeds: Vec<_> = video_seeds.iter().take(8).collect();
        let mut sources: Vec<Source> = vec![];
        if let Some((w, v)) = seeds.first()
            && let Ok(items) = client.related(&v.id)
        {
            sources.push(Source { weight: *w, items });
        }
        std::thread::scope(|sc| {
            let related: Vec<_> = seeds
                .iter()
                .skip(1)
                .map(|(w, v)| (*w, sc.spawn(|| client.related(&v.id))))
                .collect();
            let searches: Vec<_> = query_seeds
                .iter()
                .take(query_count)
                .map(|(w, q)| (*w * 0.8, sc.spawn(|| search(q))))
                .collect();
            for (weight, h) in related.into_iter().chain(searches) {
                if let Ok(Ok(items)) = h.join() {
                    sources.push(Source { weight, items });
                }
            }
        });

        // Anything already watched, skipped or dismissed stays out.
        let mut exclude = self.hidden.clone();
        exclude.extend(
            self.signals
                .iter()
                .filter_map(|s| s.video.as_ref().map(|v| v.id.clone())),
        );
        let out = rank(
            &sources,
            &channel_aff,
            &terms,
            &exclude,
            &self.impressions,
            size,
            t / 60,
        );
        (out, client.visitor.into_inner().unwrap())
    }
}

/// The scoring and mixing step, kept pure so it can be tested.
pub fn rank(
    sources: &[Source],
    channel_aff: &HashMap<String, f64>,
    terms: &HashMap<String, f64>,
    exclude: &HashSet<String>,
    impressions: &HashMap<String, u32>,
    size: usize,
    seed: u64,
) -> Vec<VideoItem> {
    let mut items: HashMap<&str, &VideoItem> = HashMap::new();
    let mut score: HashMap<&str, f64> = HashMap::new();
    let mut votes: HashMap<&str, u32> = HashMap::new();
    for src in sources {
        for (r, v) in src.items.iter().enumerate() {
            if exclude.contains(&v.id) {
                continue;
            }
            items.entry(&v.id).or_insert(v);
            // Higher in a related list = closer to the seed.
            *score.entry(&v.id).or_default() += src.weight / (1.0 + 0.12 * r as f64);
            *votes.entry(&v.id).or_default() += 1;
        }
    }
    let top_term = terms.values().cloned().fold(0.0, f64::max);
    for (id, v) in &items {
        let mut s = score[id];
        // Pointed at by several seeds: very likely on-taste.
        s *= 1.0 + 0.25 * (votes[id] - 1) as f64;
        let aff = channel_aff.get(&v.channel_key()).copied().unwrap_or(0.0);
        s += 0.35 * aff.clamp(-2.0, 2.0);
        if top_term > 0.0 {
            let overlap: f64 = tokens(&v.title)
                .iter()
                .map(|t| terms.get(t).copied().unwrap_or(0.0) / top_term)
                .sum();
            s *= 1.0 + (0.2 * overlap).min(0.6);
        }
        s *= 0.7f64.powi(impressions.get(*id).copied().unwrap_or(0) as i32);
        score.insert(id, s);
    }

    let mut ranked: Vec<&str> = items.keys().copied().filter(|id| score[id] > 0.0).collect();
    ranked.sort_by(|a, b| score[b].total_cmp(&score[a]).then(a.cmp(b)));

    // Which candidates each source brought, best first: wildcard slots reach
    // back to older or weaker interests instead of only the top one.
    let by_source: Vec<Vec<&str>> = sources
        .iter()
        .map(|src| {
            ranked
                .iter()
                .copied()
                .filter(|id| src.items.iter().any(|v| v.id == *id))
                .collect()
        })
        .collect();
    let strongest =
        (0..sources.len()).max_by(|a, b| sources[*a].weight.total_cmp(&sources[*b].weight));

    let mut rng = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };

    let mut out: Vec<VideoItem> = vec![];
    let mut per_channel: HashMap<String, i32> = HashMap::new();
    let mut pool: Vec<&str> = ranked.clone();
    while out.len() < size && !pool.is_empty() {
        // Every 5th slot: the best remaining pick of another source.
        let side = if out.len() % 5 == 4 {
            let options: Vec<usize> = (0..sources.len())
                .filter(|i| {
                    Some(*i) != strongest && by_source[*i].iter().any(|id| pool.contains(id))
                })
                .collect();
            if options.is_empty() {
                None
            } else {
                let weights: Vec<f64> = options
                    .iter()
                    .map(|i| sources[*i].weight.max(0.01).sqrt())
                    .collect();
                let mut roll = next() * weights.iter().sum::<f64>();
                let mut chosen = *options.last().unwrap();
                for (k, w) in weights.iter().enumerate() {
                    roll -= w;
                    if roll <= 0.0 {
                        chosen = options[k];
                        break;
                    }
                }
                by_source[chosen]
                    .iter()
                    .copied()
                    .find(|id| pool.contains(id))
            }
        } else {
            None
        };
        let pick = side.unwrap_or_else(|| {
            // Greedy, with each extra video from the same channel worth less.
            *pool
                .iter()
                .take(30)
                .max_by(|a, b| {
                    let adj = |id: &&str| {
                        score[*id]
                            * 0.55f64
                                .powi(*per_channel.get(&items[*id].channel_key()).unwrap_or(&0))
                    };
                    adj(a).total_cmp(&adj(b)).then(b.cmp(a))
                })
                .unwrap()
        });
        pool.retain(|id| *id != pick);
        let v = items[pick];
        *per_channel.entry(v.channel_key()).or_default() += 1;
        out.push(v.clone());
    }
    out
}

const STOP: [&str; 58] = [
    "the", "and", "for", "with", "you", "your", "this", "that", "from", "are", "was", "how",
    "what", "why", "who", "all", "not", "but", "can", "its", "our", "out", "new", "get", "got",
    "has", "have", "into", "official", "video", "videos", "full", "episode", "part", "feat",
    "live", "lyrics", "audio", "4k", "shorts", "just", "one", "more", "about", "they", "them",
    "his", "her", "will", "when", "ft", "hd", "vs", "day", "days", "best", "make", "made",
];

/// Lower-cased title words worth matching on (no stop words, 3+ chars).
pub fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(w))
        .map(String::from)
        .collect()
}

// ── YouTube "watch next", signed out ─────────────────────────────────────────

const ORIGIN: &str = "https://www.youtube.com";
const CLIENT_VERSION: &str = "2.20261110.01.00";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

pub struct Related {
    pub visitor: std::sync::Mutex<Option<String>>,
}

impl Related {
    /// Up to ~40 related videos (first page plus one continuation).
    pub fn related(&self, id: &str) -> Result<Vec<VideoItem>, String> {
        let (mut items, token) = parse_next(&self.post(json!({ "videoId": id }))?);
        if let Some(t) = token
            && let Ok(more) = self.post(json!({ "continuation": t }))
        {
            items.extend(parse_next(&more).0);
        }
        let mut seen = HashSet::from([id.to_string()]);
        items.retain(|v| seen.insert(v.id.clone()));
        Ok(items)
    }

    fn post(&self, body: Value) -> Result<String, String> {
        let visitor = self.visitor.lock().unwrap().clone();
        let mut client = json!({
            "clientName": "WEB", "clientVersion": CLIENT_VERSION, "hl": "en", "gl": "US",
        });
        if let Some(v) = &visitor {
            client["visitorData"] = json!(v);
        }
        let mut payload = json!({ "context": { "client": client } });
        for (k, v) in body.as_object().unwrap() {
            payload[k] = v.clone();
        }
        let mut req = ureq::post(format!("{ORIGIN}/youtubei/v1/next?prettyPrint=false"))
            .header("Content-Type", "application/json")
            .header("User-Agent", UA)
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Origin", ORIGIN)
            .header("X-Youtube-Client-Name", "1")
            .header("X-Youtube-Client-Version", CLIENT_VERSION);
        if let Some(v) = &visitor {
            req = req.header("X-Goog-Visitor-Id", v);
        }
        let text = req
            .send(payload.to_string())
            .map_err(|e| e.to_string())?
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        if visitor.is_none()
            && let Some(start) = text.find("\"visitorData\":\"")
        {
            let rest = &text[start + 15..];
            if let Some(end) = rest.find('"') {
                *self.visitor.lock().unwrap() = Some(rest[..end].to_string());
            }
        }
        Ok(text)
    }
}

fn find<'a>(node: &'a Value, key: &str) -> Option<&'a Value> {
    match node {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|v| find(v, key))),
        Value::Array(a) => a.iter().find_map(|v| find(v, key)),
        _ => None,
    }
}

/// Parses a `next` response (first page or continuation) into videos and the
/// continuation token, if any.
pub fn parse_next(text: &str) -> (Vec<VideoItem>, Option<String>) {
    let Ok(j) = serde_json::from_str::<Value>(text) else {
        return (vec![], None);
    };
    let items = j
        .pointer("/contents/twoColumnWatchNextResults/secondaryResults/secondaryResults/results")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| {
            j.get("onResponseReceivedEndpoints")?
                .as_array()?
                .iter()
                .find_map(|e| {
                    e.pointer("/appendContinuationItemsAction/continuationItems")
                        .or_else(|| e.pointer("/reloadContinuationItemsCommand/continuationItems"))
                        .and_then(Value::as_array)
                        .cloned()
                })
        })
        .unwrap_or_default();
    let mut out = vec![];
    let mut token = None;
    for x in &items {
        if let Some(lv) = x.get("lockupViewModel") {
            if let Some(v) = from_lockup(lv) {
                out.push(v);
            }
        } else if let Some(c) = x.get("continuationItemRenderer") {
            token = find(c, "token").and_then(Value::as_str).map(String::from);
        }
    }
    (out, token)
}

fn from_lockup(lv: &Value) -> Option<VideoItem> {
    if lv.get("contentType")?.as_str()? != "LOCKUP_CONTENT_TYPE_VIDEO" {
        return None; // mixes, playlists
    }
    let id = lv.get("contentId")?.as_str()?.to_string();
    let meta = lv.pointer("/metadata/lockupMetadataViewModel")?;
    let rows = meta
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let parts = |i: usize| -> Vec<String> {
        rows.get(i)
            .and_then(|r| r.get("metadataParts"))
            .and_then(Value::as_array)
            .map(|p| {
                p.iter()
                    .filter_map(|x| x.pointer("/text/content")?.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let row1 = parts(1);
    let badge = lv
        .pointer("/contentImage/thumbnailViewModel/overlays")
        .and_then(Value::as_array)
        .and_then(|o| {
            o.iter().find_map(|x| {
                x.pointer("/thumbnailBottomOverlayViewModel/badges")?
                    .as_array()?
                    .iter()
                    .find_map(|b| b.pointer("/thumbnailBadgeViewModel/text")?.as_str())
            })
        })
        .map(String::from);
    let live = badge
        .as_deref()
        .is_some_and(|b| b.eq_ignore_ascii_case("LIVE"))
        || row1.iter().any(|s| s.contains("watching"));
    let channel_id = meta
        .get("image")
        .and_then(|i| find(i, "browseId"))
        .or_else(|| meta.get("metadata").and_then(|m| find(m, "browseId")))
        .and_then(Value::as_str)
        .filter(|c| c.starts_with("UC"))
        .map(String::from);
    Some(VideoItem {
        title: meta.pointer("/title/content")?.as_str()?.to_string(),
        channel: parts(0).into_iter().next().unwrap_or_default(),
        channel_id,
        thumbnail: format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),
        duration: if live {
            None
        } else {
            badge.as_deref().and_then(parse_clock)
        },
        views: row1.first().and_then(|s| parse_count(s)),
        live,
        short: false,
        id,
    })
}

/// "3:24" / "1:02:03" → seconds.
pub fn parse_clock(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(
        parts
            .iter()
            .fold(0.0, |acc, p| acc * 60.0 + p.parse::<f64>().unwrap()),
    )
}

/// "208M" / "1.3B views" / "9,412 views" → a number.
pub fn parse_count(s: &str) -> Option<u64> {
    let num: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    let n: f64 = num.replace(',', "").parse().ok()?;
    let mult = match s[num.len()..].trim_start().chars().next() {
        Some('K' | 'k') => 1e3,
        Some('M') => 1e6,
        Some('B') => 1e9,
        _ => 1.0,
    };
    Some((n * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: &str, channel: &str, title: &str) -> VideoItem {
        VideoItem {
            id: id.into(),
            title: title.into(),
            channel: channel.into(),
            channel_id: Some(format!("UC{channel}")),
            thumbnail: String::new(),
            duration: Some(600.0),
            views: None,
            live: false,
            short: false,
        }
    }

    fn ids(out: &[VideoItem]) -> Vec<&str> {
        out.iter().map(|v| v.id.as_str()).collect()
    }

    fn rank_simple(sources: &[Source]) -> Vec<VideoItem> {
        rank(
            sources,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
            60,
            1,
        )
    }

    #[test]
    fn shared_candidates_win() {
        let out = rank_simple(&[
            Source {
                weight: 1.0,
                items: vec![v("a", "x", "t"), v("shared", "y", "t")],
            },
            Source {
                weight: 1.0,
                items: vec![v("b", "z", "t"), v("shared", "y", "t")],
            },
        ]);
        assert_eq!(out[0].id, "shared");
    }

    #[test]
    fn excludes_and_impressions() {
        let src = [Source {
            weight: 1.0,
            items: vec![v("a", "x", "t"), v("b", "y", "t"), v("c", "z", "t")],
        }];
        let out = rank(
            &src,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::from(["b".into()]),
            &HashMap::new(),
            60,
            1,
        );
        assert!(!ids(&out).contains(&"b"));
        let out = rank(
            &src,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::from([("a".into(), 3)]),
            60,
            1,
        );
        assert_eq!(out[0].id, "b");
    }

    #[test]
    fn spreads_channels() {
        let mut items: Vec<_> = (0..6).map(|i| v(&format!("s{i}"), "big", "t")).collect();
        items.push(v("other", "small", "t"));
        let out = rank_simple(&[Source { weight: 1.0, items }]);
        assert!(out.iter().position(|v| v.id == "other").unwrap() < 3);
    }

    #[test]
    fn affinity_and_terms() {
        let src = [Source {
            weight: 1.0,
            items: vec![
                v("a", "meh", "cooking pasta"),
                v("b", "fav", "speedrun mario"),
            ],
        }];
        let aff = HashMap::from([("UCmeh".into(), -1.5), ("UCfav".into(), 1.5)]);
        assert_eq!(
            rank(
                &src,
                &aff,
                &HashMap::new(),
                &HashSet::new(),
                &HashMap::new(),
                60,
                1
            )[0]
            .id,
            "b"
        );
        let terms = HashMap::from([("speedrun".into(), 2.0)]);
        assert_eq!(
            rank(
                &src,
                &HashMap::new(),
                &terms,
                &HashSet::new(),
                &HashMap::new(),
                60,
                1
            )[0]
            .id,
            "b"
        );
    }

    #[test]
    fn side_sources_get_slots() {
        let main: Vec<_> = (0..20)
            .map(|i| v(&format!("m{i}"), &format!("c{i}"), "t"))
            .collect();
        let out = rank_simple(&[
            Source {
                weight: 2.0,
                items: main,
            },
            Source {
                weight: 0.3,
                items: vec![v("old", "o", "t")],
            },
        ]);
        assert!(out.iter().position(|v| v.id == "old").unwrap() <= 4);
    }

    #[test]
    fn signals() {
        let mut f = ForYou::default();
        f.watched(&v("a", "x", "t"), Duration::from_secs(8));
        f.watched(&v("b", "x", "t"), Duration::from_secs(590));
        assert_eq!(f.signals[0].kind, SignalKind::Skip);
        assert_eq!(f.signals[1].kind, SignalKind::Watch);
        assert!(f.signals[1].strength > 1.8);
        f.searched("https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(f.signals.len(), 2);
        f.set_interests(&["Music".into()]);
        assert_eq!(f.interests(), vec!["Music".to_string()]);
        assert!(f.has_taste());
    }

    #[test]
    fn parsing() {
        assert_eq!(parse_clock("3:24"), Some(204.0));
        assert_eq!(parse_clock("1:02:03"), Some(3723.0));
        assert_eq!(parse_clock("LIVE"), None);
        assert_eq!(parse_count("208M"), Some(208_000_000));
        assert_eq!(parse_count("1.3B views"), Some(1_300_000_000));
        assert_eq!(parse_count("9,412 views"), Some(9412));
        let lockup = |id: &str, kind: &str, badge: &str, row1: Value| {
            json!({ "lockupViewModel": {
                "contentType": kind, "contentId": id,
                "contentImage": { "thumbnailViewModel": { "overlays": [
                    { "thumbnailBottomOverlayViewModel": { "badges": [ { "thumbnailBadgeViewModel": { "text": badge } } ] } }
                ] } },
                "metadata": { "lockupMetadataViewModel": {
                    "title": { "content": "Title" },
                    "image": { "browseEndpoint": { "browseId": "UCabc" } },
                    "metadata": { "contentMetadataViewModel": { "metadataRows": [
                        { "metadataParts": [ { "text": { "content": "Artist" } } ] },
                        { "metadataParts": row1 }
                    ] } }
                } }
            } })
        };
        let text = json!({ "contents": { "twoColumnWatchNextResults": { "secondaryResults": { "secondaryResults": { "results": [
            lockup("aaaaaaaaaaa", "LOCKUP_CONTENT_TYPE_VIDEO", "3:24", json!([{ "text": { "content": "208M" } }, { "text": { "content": "17y ago" } }])),
            lockup("PLmix", "LOCKUP_CONTENT_TYPE_PLAYLIST", "50 videos", json!([])),
            lockup("bbbbbbbbbbb", "LOCKUP_CONTENT_TYPE_VIDEO", "LIVE", json!([{ "text": { "content": "1.2K watching" } }])),
            { "continuationItemRenderer": { "continuationEndpoint": { "continuationCommand": { "token": "TOKEN" } } } }
        ] } } } } })
        .to_string();
        let (items, token) = parse_next(&text);
        assert_eq!(ids(&items), vec!["aaaaaaaaaaa", "bbbbbbbbbbb"]);
        assert_eq!(items[0].channel, "Artist");
        assert_eq!(items[0].channel_id.as_deref(), Some("UCabc"));
        assert_eq!(items[0].duration, Some(204.0));
        assert_eq!(items[0].views, Some(208_000_000));
        assert!(items[1].live);
        assert_eq!(token.as_deref(), Some("TOKEN"));
    }

    /// Live check against YouTube: `cargo test -- --ignored live --nocapture`
    #[test]
    #[ignore]
    fn live() {
        let client = Related {
            visitor: std::sync::Mutex::new(None),
        };
        let items = client.related("dQw4w9WgXcQ").unwrap();
        println!(
            "{} related, visitor set: {}",
            items.len(),
            client.visitor.lock().unwrap().is_some()
        );
        for v in items.iter().take(5) {
            println!("  {} | {} | {:?}", v.title, v.channel, v.duration);
        }
        assert!(items.len() >= 10);
        let mut f = ForYou::default();
        f.watched(&items[0], Duration::from_secs(400));
        let (out, _) = f.build(|_| Ok(vec![]), 20);
        println!("feed after watching {}:", items[0].title);
        for v in &out {
            println!("  {} | {}", v.title, v.channel);
        }
        assert!(out.len() >= 10);
    }
}
