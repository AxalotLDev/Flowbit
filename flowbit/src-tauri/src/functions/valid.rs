use regex::Regex;

#[tauri::command]
pub fn is_youtube_url(text: String) -> bool {
    let youtube_regex = Regex::new(
        r"^(https?://)?(www\.)?(youtube\.com|youtu\.be)/.+$"
    ).unwrap();

    youtube_regex.is_match(&text)
}