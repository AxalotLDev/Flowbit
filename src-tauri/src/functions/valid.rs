use once_cell::sync::Lazy;
use regex::Regex;

static YOUTUBE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(https?://)?(www\.)?(youtube\.com|youtu\.be)/.+$").unwrap());
static TWITCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^https?://(www\.)?(twitch\.tv/(?:videos/\d+|[\w-]+/v/\d+|clips/[\w-]+)|clips\.twitch\.tv/[\w-]+)$").unwrap());
static TIME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d{2}:\d{2}:\d{2}$").unwrap());

#[tauri::command]
pub fn is_youtube_url(url: String) -> bool {
    YOUTUBE_RE.is_match(&url)
}

#[tauri::command]
pub fn is_twitch_url(url: String) -> bool {
    TWITCH_RE.is_match(url.trim())
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
        Some(e) if !e.trim().is_empty() => Some(parse_time_strict(e)?),
        _ => None,
    };

    // No end, or an explicit "00:00:00" end, both mean "play to the end of
    // the video" — there's no explicit end to compare start against.
    let end_is_default = matches!(end_sec, None | Some(0));

    if start_sec == 0 && end_is_default {
        return Ok(());
    }

    if let Some(end_sec) = end_sec.filter(|_| !end_is_default) {
        if start_sec >= end_sec {
            return Err("Start must be earlier than end".into());
        }
        if let Some(max) = max_duration {
            if end_sec > max {
                return Err("End exceeds the video's duration".into());
            }
        }
    } else if let Some(max) = max_duration {
        if start_sec >= max {
            return Err("Start exceeds the video's duration".into());
        }
    }

    Ok(())
}

fn parse_time_strict(t: &str) -> Result<u64, String> {
    if !TIME_RE.is_match(t) {
        return Err("Time format must be HH:MM:SS (e.g. 00:05:30)".into());
    }

    let parts: Vec<u64> = t.split(':').map(|p| p.parse::<u64>().unwrap()).collect();

    let (h, m, s) = (parts[0], parts[1], parts[2]);

    if m >= 60 || s >= 60 {
        return Err("Minutes and seconds must be less than 60".into());
    }

    Ok(h * 3600 + m * 60 + s)
}
