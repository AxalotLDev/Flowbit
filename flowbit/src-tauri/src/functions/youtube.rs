use crate::functions::dependencies::{ffmpeg, quickjs, yt_dlp};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Serialize, Clone)]
pub struct DownloadResult {
    pub path: String,
    pub file_size_mb: f64,
}

pub struct DownloadState;
impl DownloadState {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Best,
    High,
    Medium,
    Low,
    Worst,
}

#[derive(Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum DownloadMode {
    Video,
    Audio,
}

fn default_downloads() -> Option<PathBuf> {
    dirs::download_dir()
}

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }
    let s = out.trim();
    if s.is_empty() {
        return "video".into();
    }
    if s.len() > 200 {
        s[..200].to_string()
    } else {
        s.to_string()
    }
}

#[inline]
pub fn quality_to_format(q: Quality) -> &'static str {
    match q {
        Quality::Best => "bestvideo+bestaudio/best",
        Quality::High => "bestvideo[height<=1080]+bestaudio/best[height<=1080]",
        Quality::Medium => "bestvideo[height<=720]+bestaudio/best[height<=720]",
        Quality::Low => "bestvideo[height<=480]+bestaudio/best[height<=480]",
        Quality::Worst => "worst",
    }
}

async fn fetch_title(url: &str) -> Result<String, String> {
    let args: Vec<String> = vec![
        "--print".into(),
        "title".into(),
        "--no-playlist".into(),
        url.into(),
    ];
    let output = run_ytdlp_output(args, "Failed to fetch title:".to_string(), None).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_time_to_secs(t: &str) -> Option<u64> {
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h = parts[0].parse::<u64>().ok()?;
    let m = parts[1].parse::<u64>().ok()?;
    let s = parts[2].parse::<u64>().ok()?;
    if m >= 60 || s >= 60 {
        return None;
    }
    Some(h * 3600 + m * 60 + s)
}

pub fn section_changed(start: &str, end: &str, duration: Option<u64>) -> bool {
    let start_secs = parse_time_to_secs(start).unwrap_or(0);
    if start_secs != 0 {
        return true;
    }
    let end_secs = match parse_time_to_secs(end) {
        Some(s) => s,
        None => return true,
    };
    if end_secs == 0 {
        return false;
    }
    match duration {
        Some(dur) => end_secs < dur.saturating_sub(1),
        None => true,
    }
}

pub async fn fetch_duration(url: &str) -> Option<u64> {
    let args: Vec<String> = vec![
        "--print".into(),
        "duration".into(),
        "--no-playlist".into(),
        url.into(),
    ];
    let output = run_ytdlp_output(args, "Failed to fetch duration".to_string(), None)
        .await
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .map(|d| d as u64)
}

pub async fn cleanup_temp(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let should_remove = name.ends_with(".part")
            || name.ends_with(".tmp")
            || name.ends_with(".frag")
            || name.starts_with("temp_");
        if should_remove {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

pub fn file_mb(len: u64) -> f64 {
    len as f64 / 1_048_576.0
}

fn emit_log(app: &Option<AppHandle>, line: &str) {
    if let Some(a) = app {
        let _ = a.emit("ytdlp-log", line);
    }
}

/// Запускает yt-dlp, стримит stdout и stderr как события "ytdlp-log",
/// возвращает ExitStatus.
pub async fn run_ytdlp_status(
    args: Vec<String>,
    error_format: String,
    app: Option<AppHandle>,
) -> Result<std::process::ExitStatus, String> {
    let default_args: Vec<String> = vec![
        "--ffmpeg-location".into(),
        ffmpeg().into(),
        "--js-runtimes".into(),
        format!("quickjs:{}", quickjs()),
    ];

    let mut child = Command::new(yt_dlp())
        .args(default_args.iter().chain(args.iter()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{error_format}: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let app_out = app.clone();
    let app_err = app.clone();

    let h_out = tokio::spawn(async move {
        if let Some(s) = stdout {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit_log(&app_out, &line);
            }
        }
    });

    let h_err = tokio::spawn(async move {
        if let Some(s) = stderr {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit_log(&app_err, &line);
            }
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("{error_format}: {e}"))?;

    let _ = h_out.await;
    let _ = h_err.await;

    Ok(status)
}

/// Запускает yt-dlp, стримит stderr как "ytdlp-log", собирает stdout в память,
/// возвращает Output (нужен для команд, читающих --print).
pub async fn run_ytdlp_output(
    args: Vec<String>,
    error_format: String,
    app: Option<AppHandle>,
) -> Result<std::process::Output, String> {
    let default_args: Vec<String> = vec![
        "--ffmpeg-location".into(),
        ffmpeg().into(),
        "--js-runtimes".into(),
        format!("quickjs:{}", quickjs()),
    ];

    let mut child = Command::new(yt_dlp())
        .args(default_args.iter().chain(args.iter()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{error_format}: {e}"))?;

    let stderr = child.stderr.take();
    let app_err = app.clone();

    let h_err = tokio::spawn(async move {
        if let Some(s) = stderr {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit_log(&app_err, &line);
            }
        }
    });

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("{error_format}: {e}"))?;

    let _ = h_err.await;

    Ok(output)
}

#[tauri::command]
pub async fn download_video(
    app: AppHandle,
    url: String,
    path: Option<String>,
    quality: Option<Quality>,
    mode: Option<DownloadMode>,
    start: Option<String>,
    end: Option<String>,
    duration: Option<u64>,
) -> Result<DownloadResult, String> {
    let app = Some(app);

    let out_dir = path
        .map(PathBuf::from)
        .or_else(default_downloads)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    if matches!(mode, Some(DownloadMode::Audio)) {
        return download_audio(&url, &out_dir, start, end, duration, app).await;
    }

    let title = fetch_title(&url).await?;
    let filename = sanitize_filename(&title);
    let out_file = out_dir.join(format!("{filename}.mp4"));

    let start_str = start.as_deref().unwrap_or("00:00:00").to_string();

    let resolved_duration = match duration {
        Some(d) => Some(d),
        None => fetch_duration(&url).await,
    };

    let end_str = match end.as_deref() {
        Some(e) if e != "00:00:00" => e.to_string(),
        _ => match resolved_duration {
            Some(dur) => {
                let h = dur / 3600;
                let m = (dur % 3600) / 60;
                let s = dur % 60;
                format!("{:02}:{:02}:{:02}", h, m, s)
            }
            None => "00:00:00".to_string(),
        },
    };

    let need_section = section_changed(&start_str, &end_str, resolved_duration);

    let format = quality_to_format(quality.unwrap_or(Quality::Best));
    let out_tmpl = out_dir
        .join(format!("{filename}.%(ext)s"))
        .to_string_lossy()
        .into_owned();

    let mut args: Vec<String> = vec![
        "-f".into(),
        format.into(),
        "--merge-output-format".into(),
        "mp4".into(),
        "--no-playlist".into(),
    ];
    args.push("-o".into());
    args.push(out_tmpl);
    args.push(url.clone());

    let status =
        run_ytdlp_status(args, "Failed to get status: {e}".to_string(), app.clone()).await?;

    cleanup_temp(&out_dir).await;

    if !status.success() {
        return Err("Video download failed".into());
    }

    if need_section {
        let temp_input = out_file.clone();
        let clipped_file = out_dir.join(format!("{filename}_cut.mp4"));

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            temp_input.to_string_lossy().into_owned(),
            "-ss".into(),
            start_str.clone(),
            "-to".into(),
            end_str.clone(),
            "-c".into(),
            "copy".into(),
            clipped_file.to_string_lossy().into_owned(),
        ];

        if let Some(ref a) = app {
            let _ = a.emit("ytdlp-log", "[ffmpeg] Trimming video…");
        }

        let ffmpeg_status = Command::new(ffmpeg())
            .args(&ffmpeg_args)
            .status()
            .await
            .map_err(|e| format!("ffmpeg failed: {e}"))?;

        if !ffmpeg_status.success() {
            return Err("ffmpeg trimming failed".into());
        }

        let _ = tokio::fs::remove_file(&temp_input).await;
        let _ = tokio::fs::rename(&clipped_file, &out_file).await;
    }

    let meta = tokio::fs::metadata(&out_file)
        .await
        .map_err(|e| format!("Cannot read file: {e}"))?;

    if meta.len() == 0 {
        return Err("File is empty".into());
    }

    Ok(DownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: file_mb(meta.len()),
    })
}

async fn download_audio(
    url: &str,
    out_dir: &Path,
    start: Option<String>,
    end: Option<String>,
    duration: Option<u64>,
    app: Option<AppHandle>,
) -> Result<DownloadResult, String> {
    let title = fetch_title(url).await?;
    let filename = sanitize_filename(&title);

    let out_tmpl = out_dir
        .join(format!("{filename}.%(ext)s"))
        .to_string_lossy()
        .into_owned();

    let start_str = start.as_deref().unwrap_or("00:00:00").to_string();

    let resolved_duration = match duration {
        Some(d) => Some(d),
        None => fetch_duration(url).await,
    };

    let end_str = match end.as_deref() {
        Some(e) if e != "00:00:00" => e.to_string(),
        _ => match resolved_duration {
            Some(dur) => {
                let h = dur / 3600;
                let m = (dur % 3600) / 60;
                let s = dur % 60;
                format!("{:02}:{:02}:{:02}", h, m, s)
            }
            None => "00:00:00".to_string(),
        },
    };

    let need_section = section_changed(&start_str, &end_str, resolved_duration);

    let args: Vec<String> = vec![
        "-f".into(),
        "bestaudio".into(),
        "-x".into(),
        "--audio-format".into(),
        "mp3".into(),
        "--audio-quality".into(),
        "0".into(),
        "--no-playlist".into(),
        "-o".into(),
        out_tmpl,
        "--print".into(),
        "after_move:filepath".into(),
        url.to_string(),
    ];

    let output = run_ytdlp_output(args, "yt-dlp error:".to_string(), app.clone()).await?;

    cleanup_temp(out_dir).await;

    if !output.status.success() {
        return Err("Audio download failed".into());
    }

    let real_path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string();

    if real_path.is_empty() {
        return Err("Cannot resolve output file path from yt-dlp".into());
    }

    let out_file = PathBuf::from(&real_path);

    if need_section {
        let temp_input = out_file.clone();
        let clipped_file = out_dir.join(format!("{filename}_cut.mp3"));

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            temp_input.to_string_lossy().into_owned(),
            "-ss".into(),
            start_str.clone(),
            "-to".into(),
            end_str.clone(),
            "-c".into(),
            "copy".into(),
            clipped_file.to_string_lossy().into_owned(),
        ];

        if let Some(ref a) = app {
            let _ = a.emit("ytdlp-log", "[ffmpeg] Trimming audio…");
        }

        let ffmpeg_status = Command::new(ffmpeg())
            .args(&ffmpeg_args)
            .status()
            .await
            .map_err(|e| format!("ffmpeg failed: {e}"))?;

        if !ffmpeg_status.success() {
            return Err("ffmpeg trimming failed".into());
        }

        let _ = tokio::fs::remove_file(&temp_input).await;
        let _ = tokio::fs::rename(&clipped_file, &out_file).await;
    }

    let meta = tokio::fs::metadata(&out_file)
        .await
        .map_err(|e| format!("Cannot read file: {e}"))?;

    if meta.len() == 0 {
        return Err("File is empty".into());
    }

    Ok(DownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: file_mb(meta.len()),
    })
}
