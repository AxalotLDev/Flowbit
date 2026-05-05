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

#[tauri::command]
pub fn validate_time_range(
    start: Option<String>,
    end: Option<String>,
    max_duration: Option<u64>,
) -> Result<(), String> {
    let start_sec = match start.as_deref() {
        Some(s) if !s.trim().is_empty() => parse_time_strict(s)?,
        _ => 0,
    };

    let end_sec = match end.as_deref() {
        Some(e) if !e.trim().is_empty() => parse_time_strict(e)?,
        _ => return Ok(()),
    };

    if start_sec == 0 && end_sec == 0 {
        return Ok(());
    }

    if end_sec > 0 && start_sec >= end_sec {
        return Err("Начало должно быть меньше конца".into());
    }

    if let Some(max) = max_duration {
        if end_sec > max {
            return Err("Конец превышает длительность видео".into());
        }
    }

    Ok(())
}

fn parse_time_strict(t: &str) -> Result<u64, String> {
    let re = Regex::new(r"^\d{2}:\d{2}:\d{2}$").unwrap();

    if !re.is_match(t) {
        return Err("Формат времени должен быть HH:MM:SS (например 00:05:30)".into());
    }

    let parts: Vec<u64> = t.split(':').map(|p| p.parse::<u64>().unwrap()).collect();

    let (h, m, s) = (parts[0], parts[1], parts[2]);

    if m >= 60 || s >= 60 {
        return Err("Минуты и секунды должны быть меньше 60".into());
    }

    Ok(h * 3600 + m * 60 + s)
}
