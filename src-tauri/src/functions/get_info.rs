use crate::functions::twitch::{fetch_json, TwitchVideoInfo};
use crate::functions::youtube::fetch_yt_meta;
use reqwest::Client;
use serde::Serialize;
use std::sync::OnceLock;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("Failed to build HTTP client")
    })
}

#[derive(Serialize, Clone)]
pub struct VideoInfo {
    pub title: String,
    pub author_name: String,
    pub thumbnail_url: String,
    pub html: String,
    pub duration: Option<u64>,
    pub audio_tracks: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
}

#[tauri::command]
pub async fn get_youtube_info(app: tauri::AppHandle, url: String) -> Result<VideoInfo, String> {
    let key = crate::functions::cache::video_key(&url);
    if let Some(cached) = crate::functions::cache::VIDEO_INFO_CACHE.get(&key) {
        return Ok(cached);
    }

    let oembed_url = format!("https://www.youtube.com/oembed?url={}&format=json", url);

    // oembed (fast title/author/thumbnail) and a single -J call (duration +
    // audio tracks + fallback metadata) run in parallel — the card and the
    // track-selection block appear together. If oembed is unavailable (401/404
    // for embed-restricted or age-restricted videos), fall back to yt-dlp
    // instead of failing outright.
    let (oembed_res, meta) =
        tokio::join!(client().get(&oembed_url).send(), fetch_yt_meta(&url, Some(app)));
    let oembed_json = match oembed_res {
        Ok(res) if res.status().is_success() => res.json::<serde_json::Value>().await.ok(),
        _ => None,
    };

    let title = oembed_json
        .as_ref()
        .and_then(|j| j["title"].as_str())
        .map(String::from)
        .or(meta.title)
        .unwrap_or_default();
    let author_name = oembed_json
        .as_ref()
        .and_then(|j| j["author_name"].as_str())
        .map(String::from)
        .or(meta.author)
        .unwrap_or_default();
    let thumbnail_url = oembed_json
        .as_ref()
        .and_then(|j| j["thumbnail_url"].as_str())
        .map(String::from)
        .or(meta.thumbnail)
        .unwrap_or_default();
    let html = oembed_json
        .as_ref()
        .and_then(|j| j["html"].as_str())
        .unwrap_or("")
        .to_string();

    // Only an error when we got nothing at all from either source.
    // Propagate yt-dlp's actual reason (e.g. "Sign in to confirm you're not a
    // bot") instead of a generic message — otherwise the user can't tell the
    // video needs sign-in/is unavailable from the app being broken.
    if title.is_empty() && thumbnail_url.is_empty() {
        return Err(meta.error.unwrap_or_else(|| "Failed to fetch video info".into()));
    }

    let info = VideoInfo {
        title,
        author_name,
        thumbnail_url,
        html,
        duration: meta.duration,
        audio_tracks: meta.audio_tracks,
        video_codecs: meta.video_codecs,
        audio_codecs: meta.audio_codecs,
    };
    // Only cache a complete result — oembed can succeed (title/thumbnail)
    // while the yt-dlp metadata call independently fails, leaving
    // duration: None. Caching that would lock in a bogus "00:00:00" max
    // clip length for the full TTL instead of letting the next request retry.
    if info.duration.is_some() {
        crate::functions::cache::VIDEO_INFO_CACHE.insert(key, info.clone());
    }
    Ok(info)
}

#[tauri::command]
pub async fn get_twitch_info(url: String) -> Result<TwitchVideoInfo, String> {
    let key = crate::functions::cache::video_key(&url);
    if let Some(cached) = crate::functions::cache::TWITCH_INFO_CACHE.get(&key) {
        return Ok(cached);
    }

    let json = fetch_json(&url).await?;

    let is_live = json["is_live"].as_bool().unwrap_or(false);
    let audio_tracks = crate::functions::youtube::parse_audio_langs(&json);
    let video_codecs = crate::functions::youtube::parse_video_codecs(&json);
    let audio_codecs = crate::functions::youtube::parse_audio_codecs(&json);

    let info = TwitchVideoInfo {
        title: json["title"].as_str().unwrap_or("Twitch VOD").into(),
        channel: json["uploader"]
            .as_str()
            .or_else(|| json["channel"].as_str())
            .or_else(|| json["uploader_id"].as_str())
            .unwrap_or("Unknown")
            .into(),
        is_live,
        duration: if is_live {
            None
        } else {
            json["duration"].as_f64().map(|d| d as u64)
        },
        thumbnail_url: json["thumbnail"].as_str().map(String::from),
        view_count: json["view_count"].as_u64(),
        audio_tracks,
        video_codecs,
        audio_codecs,
    };
    // Live entries have no fixed duration/view-count — don't lock in
    // ephemeral data for the TTL window.
    if !is_live {
        crate::functions::cache::TWITCH_INFO_CACHE.insert(key, info.clone());
    }
    Ok(info)
}
