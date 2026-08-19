use crate::functions::youtube::{file_mb, resolve_out_dir, DownloadResult};
use std::path::PathBuf;
use tauri::AppHandle;

/// Replaces characters invalid in filenames on Windows/macOS/Linux with `_`,
/// trims trailing dots/spaces (invalid as a Windows filename ending), and
/// caps the length so the sanitized title plus extension stays well under
/// common filesystem limits (255 bytes on most platforms).
pub fn sanitize_filename(title: &str) -> String {
    let replaced: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    let trimmed = replaced.trim().trim_end_matches(['.', ' ']);
    let truncated: String = trimmed.chars().take(200).collect();

    if truncated.is_empty() {
        "thumbnail".to_string()
    } else {
        truncated
    }
}

/// Downloads a single URL's bytes, treating any non-2xx status as an error.
async fn fetch_ok(url: &str) -> Result<Vec<u8>, String> {
    Ok(reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?
        .to_vec())
}

/// Tries `url` first; on failure (e.g. a 404 for a video with no maxres
/// thumbnail) falls back to `fallback_url` if given. Reports the primary
/// error if both fail, since it's usually the more informative one.
async fn fetch_with_fallback(url: &str, fallback_url: Option<&str>) -> Result<Vec<u8>, String> {
    match fetch_ok(url).await {
        Ok(bytes) => Ok(bytes),
        Err(primary_err) => match fallback_url {
            Some(fb) => fetch_ok(fb).await.or(Err(primary_err)),
            None => Err(primary_err),
        },
    }
}

/// Tauri command: downloads a video's preview thumbnail image to disk,
/// named after the video title (sanitized) rather than the video's own
/// output filename, since a preview can be saved independently of the
/// video/audio download.
#[tauri::command]
pub async fn download_preview(
    app: AppHandle,
    url: String,
    fallback_url: Option<String>,
    title: String,
    path: Option<String>,
) -> Result<DownloadResult, String> {
    let out_dir = resolve_out_dir(&app, path);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    let bytes = fetch_with_fallback(&url, fallback_url.as_deref()).await?;

    let out_file: PathBuf = out_dir.join(format!("{}.jpg", sanitize_filename(&title)));

    tokio::fs::write(&out_file, &bytes)
        .await
        .map_err(|e| format!("Cannot write file: {e}"))?;

    Ok(DownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: file_mb(bytes.len() as u64),
    })
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_filename;

    #[test]
    fn replaces_invalid_characters() {
        assert_eq!(sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_filename("My Video.  "), "My Video");
    }

    #[test]
    fn falls_back_to_placeholder_when_empty() {
        assert_eq!(sanitize_filename(""), "thumbnail");
        assert_eq!(sanitize_filename("   "), "thumbnail");
        assert_eq!(sanitize_filename("..."), "thumbnail");
    }

    #[test]
    fn truncates_long_titles() {
        let long = "a".repeat(500);
        assert_eq!(sanitize_filename(&long).chars().count(), 200);
    }

    #[test]
    fn keeps_ordinary_titles_untouched() {
        assert_eq!(sanitize_filename("Rick Astley - Never Gonna Give You Up"), "Rick Astley - Never Gonna Give You Up");
    }
}
