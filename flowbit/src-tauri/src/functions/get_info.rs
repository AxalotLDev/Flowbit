use crate::functions::youtube::fetch_duration;
use crate::functions::twitch::{fetch_json, TwitchVideoInfo};
use reqwest::Client;
use serde::Serialize;

#[derive(Serialize)]
pub struct VideoInfo {
    pub title: String,
    pub author_name: String,
    pub thumbnail_url: String,
    pub html: String,
    pub duration: Option<u64>,
}

#[tauri::command]
pub async fn get_youtube_info(url: String) -> Result<VideoInfo, String> {
    let client = Client::new();
    let res = client
        .get(format!(
            "https://www.youtube.com/oembed?url={}&format=json",
            url
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err("Failed to fetch video info".into());
    }

    let duration = fetch_duration(&url).await;
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
    })
}

#[tauri::command]
pub async fn get_twitch_info(url: String) -> Result<TwitchVideoInfo, String> {
    let json = fetch_json(&url).await?;

    let is_live = json["is_live"].as_bool().unwrap_or(false);

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
    })
}
