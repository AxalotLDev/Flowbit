use reqwest::Client;
use serde::Serialize;

#[derive(Serialize)]
pub struct VideoInfo {
    pub title: String,
    pub author_name: String,
    pub thumbnail_url: String,
    pub html: String,
}

#[tauri::command]
pub async fn get_youtube_info(url: String) -> Result<VideoInfo, String> {
    let client = Client::new();

    let request_url = format!(
        "https://www.youtube.com/oembed?url={}&format=json",
        url
    );

    let res = client
        .get(&request_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err("Failed to fetch video info".to_string());
    }
    let json = res
        .json::<serde_json::Value>()
        .await
        .map_err(|e: reqwest::Error| e.to_string())?;

    Ok(VideoInfo {
        title: json["title"].as_str().unwrap_or("").to_string(),
        author_name: json["author_name"].as_str().unwrap_or("").to_string(),
        thumbnail_url: json["thumbnail_url"].as_str().unwrap_or("").to_string(),
        html: json["html"].as_str().unwrap_or("").to_string(),
    })
}