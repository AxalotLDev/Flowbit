use regex::Regex;

#[tauri::command]
pub fn is_youtube_url(text: String) -> bool {
    Regex::new(r"^(https?://)?(www\.)?(youtube\.com|youtu\.be)/.+$")
        .unwrap()
        .is_match(&text)
}