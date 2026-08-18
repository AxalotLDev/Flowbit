use crate::functions::youtube::{
    fetch_duration, network_args, read_printed_path, resolve_out_dir, run_ffmpeg, run_ytdlp_output,
    section_changed, DownloadResult,
};
use crate::functions::get_info::get_twitch_info;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, State};

#[derive(Serialize, Clone)]
pub struct TwitchVideoInfo {
    pub title: String,
    pub channel: String,
    pub duration: Option<u64>,
    pub is_live: bool,
    pub thumbnail_url: Option<String>,
    pub view_count: Option<u64>,
    pub audio_tracks: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
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
        return Err(crate::functions::youtube::decode_output(&out.stderr));
    }
    serde_json::from_str(&crate::functions::youtube::decode_output(&out.stdout))
        .map_err(|e| format!("JSON error: {e}"))
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
    audio_lang: Option<String>,
    // Twitch-VOD всегда H.264/AAC — выбор кодека здесь ни на что не влияет,
    // принимаем параметры лишь ради единого фронтенд-API. Кодек аудио задаёт
    // только контейнер (на случай, если когда-нибудь появится opus).
    video_codec: Option<String>,
    audio_codec: Option<String>,
) -> Result<DownloadResult, String> {
    let _ = video_codec;
    let _guard = crate::functions::youtube::DownloadGuard::new();
    let app_opt = Some(app.clone());
    let out_dir = resolve_out_dir(&app, path);

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| e.to_string())?;

    // Валидируем URL (ошибка пробросится). Имя файла отдаём yt-dlp через
    // %(title)s — сами из title не строим, иначе кириллица бьётся (cp1251 stdout).
    let _ = get_twitch_info(url.clone()).await?;
    let is_audio = matches!(mode, Some(DownloadMode::Audio));

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

    let tmp_dir = out_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    let path_file = tmp_dir.join("__filepath.txt");
    let _ = tokio::fs::remove_file(&path_file).await;

    let mut args: Vec<String> = vec![
        "-o".into(),
        "%(title)s.%(ext)s".into(),
        "--no-playlist".into(),
        "-P".into(),
        format!("home:{}", out_dir.to_string_lossy()),
        "-P".into(),
        format!("temp:{}", tmp_dir.to_string_lossy()),
        "--print-to-file".into(),
        "after_move:filepath".into(),
        path_file.to_string_lossy().into_owned(),
    ];
    args.extend(network_args());

    let lang = audio_lang.as_deref().filter(|l| !l.is_empty());
    if is_audio {
        let af = match lang {
            Some(l) => format!("bestaudio[language={l}]/bestaudio"),
            None => "bestaudio".to_string(),
        };
        args.extend(["-f".into(), af, "-x".into(), "--audio-quality".into(), "0".into()]);
    } else {
        let base = quality.unwrap_or(TwitchQuality::Best).fmt();
        let vf = match lang {
            Some(l) => {
                let with_lang = base.replacen("+bestaudio", &format!("+bestaudio[language={l}]"), 1);
                format!("{with_lang}/{base}")
            }
            None => base.to_string(),
        };
        let container =
            crate::functions::youtube::merge_container(audio_codec.as_deref().filter(|c| !c.is_empty()));
        args.extend(["-f".into(), vf, "--merge-output-format".into(), container.into()]);
    }

    args.push(url.clone());

    let output = run_ytdlp_output(args, "yt-dlp failed:".to_string(), app_opt.clone()).await?;

    let real_path = read_printed_path(&path_file).await;

    cleanup_temp(&out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    if !output.status.success() {
        return Err("yt-dlp failed".into());
    }

    let out_file = PathBuf::from(real_path.ok_or("Cannot resolve output file path")?);

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

        let ffmpeg_status = run_ffmpeg(ffmpeg_args).await?;

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