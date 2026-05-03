use regex::Regex;

#[tauri::command]
pub fn is_youtube_url(url: String) -> bool {
    Regex::new(r"^(https?://)?(www\.)?(youtube\.com|youtu\.be)/.+$")
        .unwrap()
        .is_match(&url)
}

#[tauri::command]
pub fn is_twitch_url(url: String) -> bool {
    let re = Regex::new(r"^https?://(www\.)?twitch\.tv/videos/\d+/?$").unwrap();
    re.is_match(url.trim())
}