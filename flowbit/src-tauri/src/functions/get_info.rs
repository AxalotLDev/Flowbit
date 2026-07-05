use crate::functions::twitch::{fetch_json, TwitchVideoInfo};
use crate::functions::youtube::fetch_duration_and_tracks;
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

#[derive(Serialize)]
pub struct VideoInfo {
    pub title: String,
    pub author_name: String,
    pub thumbnail_url: String,
    pub html: String,
    pub duration: Option<u64>,
    pub audio_tracks: Vec<String>,
}

#[tauri::command]
pub async fn get_youtube_info(url: String) -> Result<VideoInfo, String> {
    let oembed_url = format!("https://www.youtube.com/oembed?url={}&format=json", url);

    // oembed (быстрые title/автор/обложка) и один -J (длительность + аудиодорожки)
    // параллельно — карточка и блок выбора дорожки готовы одновременно.
    let (oembed_res, (duration, audio_tracks)) =
        tokio::join!(client().get(&oembed_url).send(), fetch_duration_and_tracks(&url),);

    let res = oembed_res.map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err("Failed to fetch video info".into());
    }

    let json = res
        .json::<serde_json::Value>()
        .await
        .map_err(|e| e.to_string())?;

    Ok(VideoInfo {
        title: json["title"].as_str().unwrap_or("").to_string(),
        author_name: json["author_name"].as_str().unwrap_or("").to_string(),
        thumbnail_url: json["thumbnail_url"].as_str().unwrap_or("").to_string(),
        html: json["html"].as_str().unwrap_or("").to_string(),
        duration,
        audio_tracks,
    })
}

#[tauri::command]
pub async fn get_twitch_info(url: String) -> Result<TwitchVideoInfo, String> {
    let json = fetch_json(&url).await?;

    let is_live = json["is_live"].as_bool().unwrap_or(false);
    let audio_tracks = crate::functions::youtube::parse_audio_langs(&json);

    Ok(TwitchVideoInfo {
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
    })
}
