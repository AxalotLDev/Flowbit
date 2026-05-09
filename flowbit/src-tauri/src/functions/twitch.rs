use crate::functions::youtube::{fetch_duration, run_ytdlp_output, section_changed, DownloadResult};
use crate::functions::get_info::get_twitch_info;
use crate::functions::dependencies::ffmpeg;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, State};
use tokio::process::Command;

#[derive(Serialize, Clone)]
pub struct TwitchVideoInfo {
    pub title: String,
    pub channel: String,
    pub duration: Option<u64>,
    pub is_live: bool,
    pub thumbnail_url: Option<String>,
    pub view_count: Option<u64>,
}

pub struct TwitchDownloadState;
impl TwitchDownloadState {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum TwitchQuality {
    Best,
    High,
    Medium,
    Low,
    Worst,
}

impl TwitchQuality {
    #[inline]
    fn fmt(self) -> &'static str {
        match self {
            Self::Best => "bestvideo+bestaudio/best",
            Self::High => "bestvideo[height<=1080]+bestaudio/best[height<=1080]",
            Self::Medium => "bestvideo[height<=720]+bestaudio/best[height<=720]",
            Self::Low => "bestvideo[height<=480]+bestaudio/best[height<=480]",
            Self::Worst => "worstvideo+worstaudio/worst",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadMode {
    Video,
    Audio,
}

fn default_downloads() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        let downloads = home.join("Downloads");
        if downloads.exists() {
            return downloads;
        }
        return home;
    }
    std::env::current_dir().unwrap_or_else(|_| ".".into())
}

fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }
    let s = out.trim();
    if s.is_empty() {
        return "stream".into();
    }
    if s.len() > 200 {
        s[..200].to_string()
    } else {
        s.to_string()
    }
}

#[inline]
fn mb(len: u64) -> f64 {
    len as f64 / 1_048_576.0
}

async fn cleanup_temp(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let bad = name.ends_with(".part")
            || name.ends_with(".tmp")
            || name.ends_with(".frag")
            || name.starts_with("temp_");
        if bad {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

pub async fn fetch_json(url: &str) -> Result<Value, String> {
    let args: Vec<String> = vec!["--dump-json".into(), "--no-playlist".into(), url.into()];
    let out = run_ytdlp_output(args, "yt-dlp error:".to_string(), None).await?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("JSON error: {e}"))
}

#[tauri::command]
pub async fn download_twitch(
    app: AppHandle,
    _state: State<'_, TwitchDownloadState>,
    url: String,
    path: Option<String>,
    quality: Option<TwitchQuality>,
    mode: Option<DownloadMode>,
    start: Option<String>,
    end: Option<String>,
    duration: Option<u64>,
) -> Result<DownloadResult, String> {
    let app_opt = Some(app.clone());
    let out_dir = path.map(PathBuf::from).unwrap_or_else(default_downloads);

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| e.to_string())?;

    let info = get_twitch_info(url.clone()).await?;
    let name = sanitize(&info.title);
    let is_audio = matches!(mode, Some(DownloadMode::Audio));

    let out_tmpl = out_dir
        .join(format!("{name}.%(ext)s"))
        .to_string_lossy()
        .into_owned();

    let start_str = start.as_deref().unwrap_or("00:00:00").to_string();

    let resolved_duration = match duration {
        Some(d) => Some(d),
        None => fetch_duration(&url).await,
    };

    let end_str = match end {
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

    let mut args: Vec<String> = vec![
        "-o".into(),
        out_tmpl,
        "--no-playlist".into(),
        "--print".into(),
        "after_move:filepath".into(),
    ];

    if is_audio {
        args.extend(vec![
            "-f".into(),
            "bestaudio".into(),
            "-x".into(),
            "--audio-quality".into(),
            "0".into(),
        ]);
    } else {
        args.extend(vec![
            "-f".into(),
            quality.unwrap_or(TwitchQuality::Best).fmt().into(),
            "--merge-output-format".into(),
            "mp4".into(),
        ]);
        args.push("--postprocessor-args".into());
    }

    args.push(url.clone());

    let output = run_ytdlp_output(args, "yt-dlp failed:".to_string(), app_opt.clone()).await?;

    cleanup_temp(&out_dir).await;

    if !output.status.success() {
        return Err("yt-dlp failed".into());
    }

    let real_path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string();

    if real_path.is_empty() {
        return Err("Cannot resolve output file path".into());
    }

    let out_file = PathBuf::from(real_path);

    if need_section {
        let temp_input = out_file.clone();
        let clipped_file = out_file.with_file_name(format!(
            "{}_clip.{}",
            out_file.file_stem().unwrap_or_default().to_string_lossy(),
            out_file.extension().unwrap_or_default().to_string_lossy()
        ));

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

        let _ = app.emit("ytdlp-log", "[ffmpeg] Clipping stream…");

        let ffmpeg_status = Command::new(ffmpeg())
            .args(&ffmpeg_args)
            .status()
            .await
            .map_err(|e| format!("ffmpeg failed: {e}"))?;

        if !ffmpeg_status.success() {
            return Err("ffmpeg clipping failed".into());
        }
        let _ = tokio::fs::remove_file(&temp_input).await;
        let _ = tokio::fs::rename(&clipped_file, &out_file).await;
    }

    let meta = tokio::fs::metadata(&out_file)
        .await
        .map_err(|e| e.to_string())?;

    if meta.len() == 0 {
        return Err("Empty file".into());
    }

    Ok(DownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: mb(meta.len()),
    })
}