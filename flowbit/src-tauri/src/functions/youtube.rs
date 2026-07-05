use crate::functions::dependencies::{ffmpeg, quickjs, yt_dlp};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;

/// Сообщение об отмене — фронтенд распознаёт его и не показывает как ошибку.
pub const CANCEL_MSG: &str = "Загрузка отменена";

/// Флаг Windows, подавляющий всплывающее консольное окно дочернего процесса.
/// Без него при каждом запуске yt-dlp/ffmpeg мигает окно cmd.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Создаёт tokio-команду, на Windows скрывая консольное окно.
/// kill_on_drop гарантирует, что процесс будет убит, если его future
/// уронят (используется для мгновенной отмены).
pub fn new_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.kill_on_drop(true);
    // Заставляем Python (yt-dlp) выводить UTF-8. На Windows stdout пайпа иначе
    // кодируется в ANSI-кодовой странице (cp1251), и кириллица в выводе/путях
    // превращается в «ромбики» (U+FFFD) при декодировании как UTF-8.
    cmd.env("PYTHONUTF8", "1");
    cmd.env("PYTHONIOENCODING", "utf-8");
    // Свой process group: yt-dlp (PyInstaller) форкает дочерний воркер; убить
    // надо всю группу, а не только бутлоадер (иначе воркер продолжает качать).
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Убивает всё дерево/группу процессов дочернего процесса по его PID.
/// На Unix процесс запущен как лидер группы (process_group(0)), поэтому
/// killpg(pid) убивает и бутлоадер, и воркер. На Windows — taskkill /T.
fn kill_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

/// Глобальный сигнал отмены текущей загрузки: флаг + Notify.
/// Флаг ловит отмену, нажатую в момент, когда задача ещё не вошла в select!
/// (Notify::notify_waiters будит только уже зарегистрированных ждущих).
static CANCEL: Lazy<Notify> = Lazy::new(Notify::new);
static CANCEL_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Сбрасывает флаг отмены в начале новой загрузки.
pub fn begin_download() {
    CANCEL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
}

fn is_cancelled() -> bool {
    CANCEL_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Ждёт завершения процесса, но по сигналу отмены мгновенно его убивает.
async fn wait_cancellable(
    child: &mut tokio::process::Child,
) -> Result<std::process::ExitStatus, String> {
    // Отмену могли нажать до входа в select! — проверяем флаг заранее.
    if is_cancelled() {
        kill_group(child.id());
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(CANCEL_MSG.to_string());
    }
    tokio::select! {
        res = child.wait() => res.map_err(|e| e.to_string()),
        _ = CANCEL.notified() => {
            kill_group(child.id());
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(CANCEL_MSG.to_string())
        }
    }
}

/// Запускает ffmpeg с поддержкой мгновенной отмены.
pub async fn run_ffmpeg(args: Vec<String>) -> Result<std::process::ExitStatus, String> {
    let mut child = new_command(&ffmpeg())
        .args(&args)
        .spawn()
        .map_err(|e| format!("ffmpeg failed: {e}"))?;
    wait_cancellable(&mut child).await
}

/// Tauri-команда: мгновенно прерывает текущую загрузку. Уже скачанные файлы
/// (в т.ч. завершённые ролики плейлиста и частично скачанные фрагменты) не удаляются.
#[tauri::command]
pub fn cancel_download() {
    CANCEL_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    CANCEL.notify_waiters();
}

/// Опции устойчивости и скорости сети — аналог флагов из fish-функции `ytdl`:
/// бесконечные ретраи, докачка, параллельные фрагменты, таймауты, без mtime.
pub fn network_args() -> Vec<String> {
    [
        "--no-mtime",
        "--retries",
        "infinite",
        "--fragment-retries",
        "infinite",
        "--file-access-retries",
        "10",
        // 5 c для быстрого переподключения при коротком разрыве. Ретраи бесконечны,
        // так что застрять нельзя, а при затяжной проблеме есть ручная отмена.
        "--socket-timeout",
        "5",
        "--http-chunk-size",
        "10M",
        "--concurrent-fragments",
        "4",
        "--continue",
        "--progress",
        "--newline",
        // Не показывать предупреждение "версия старше 90 дней" на каждой загрузке;
        // за актуальностью следит фоновая проверка обновлений при старте.
        "--no-update",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Определяет каталог назначения: переданный путь → платформенный каталог
/// загрузок (в т.ч. на Android через Tauri path API) → запасные варианты.
pub fn resolve_out_dir(app: &AppHandle, path: Option<String>) -> PathBuf {
    if let Some(p) = path {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(d) = app.path().download_dir() {
        return d;
    }
    if let Some(d) = dirs::download_dir() {
        return d;
    }
    if let Ok(d) = crate::functions::dependencies::app_data_root(app) {
        return d;
    }
    std::env::current_dir().unwrap_or_else(|_| ".".into())
}

/// Проверяет наличие новой версии yt-dlp и обновляет бинарник (`yt-dlp -U`).
/// Возвращает true, если yt-dlp уже актуален либо успешно обновлён.
/// Вывод стримится во фронтенд как события "ytdlp-log".
pub async fn ytdlp_self_update(app: Option<AppHandle>) -> Result<bool, String> {
    let status = run_ytdlp_status(
        vec!["--update".into()],
        "yt-dlp update failed".to_string(),
        app,
    )
    .await?;
    Ok(status.success())
}

/// Tauri-команда: обновление yt-dlp по запросу фронтенда.
#[tauri::command]
pub async fn update_ytdlp(app: AppHandle) -> Result<bool, String> {
    ytdlp_self_update(Some(app)).await
}

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
    // Обрезаем по символам, а не по байтам: срез s[..200] паникует, если 200
    // попадает в середину многобайтового UTF-8 символа (кириллица и т.п.).
    if s.chars().count() > 200 {
        s.chars().take(200).collect()
    } else {
        s.to_string()
    }
}

#[inline]
pub fn quality_to_format(q: Quality) -> &'static str {
    // Формат в стиле fish-функции ytdl: bv*+ba/b с запасным вариантом b.
    match q {
        Quality::Best => "bv*+ba/b",
        Quality::High => "bv*[height<=1080]+ba/b[height<=1080]/b",
        Quality::Medium => "bv*[height<=720]+ba/b[height<=720]/b",
        Quality::Low => "bv*[height<=480]+ba/b[height<=480]/b",
        Quality::Worst => "wv*+wa/w",
    }
}

/// Клиент YouTube, раскрывающий мультиязычные аудиодорожки. Дефолтный клиент
/// отдаёт только оригинальную дорожку; web_embedded — все дубляжи. Указываем оба,
/// чтобы видео осталось в максимальном качестве (до 4K), а аудио — на всех языках.
const YT_MULTI_AUDIO_CLIENT: &str = "youtube:player_client=default,web_embedded";

/// Формат видео с учётом выбранной аудиодорожки (языка). Если язык задан —
/// предпочитаем аудио на этом языке, с откатом на обычный выбор.
fn video_format_with_lang(q: Quality, audio_lang: Option<&str>) -> String {
    let base = quality_to_format(q);
    match audio_lang {
        Some(l) if !l.is_empty() => {
            // Селектор видео под выбранное качество. Аудио берём строго на нужном
            // языке (ba[language=..], без разрешения), с полным fallback дальше.
            let vsel = match q {
                Quality::Best => "bv*",
                Quality::High => "bv*[height<=1080]",
                Quality::Medium => "bv*[height<=720]",
                Quality::Low => "bv*[height<=480]",
                Quality::Worst => "wv*",
            };
            format!("{vsel}+ba[language={l}]/{base}")
        }
        _ => base.to_string(),
    }
}

/// Формат для аудио-режима с учётом языка дорожки.
fn audio_format_with_lang(audio_lang: Option<&str>) -> String {
    match audio_lang {
        Some(l) if !l.is_empty() => format!("bestaudio[language={l}]/bestaudio"),
        _ => "bestaudio".to_string(),
    }
}

/// Извлекает коды языков аудиодорожек из JSON yt-dlp (уникальные, по порядку).
pub fn parse_audio_langs(json: &serde_json::Value) -> Vec<String> {
    let mut langs: Vec<String> = Vec::new();
    if let Some(formats) = json["formats"].as_array() {
        for f in formats {
            // Только аудио-дорожки (без видео) с указанным языком.
            let audio_only = f["vcodec"].as_str() == Some("none")
                && f["acodec"].as_str().is_some_and(|a| a != "none");
            if audio_only {
                if let Some(lang) = f["language"].as_str() {
                    if !lang.is_empty() && !langs.iter().any(|x| x == lang) {
                        langs.push(lang.to_string());
                    }
                }
            }
        }
    }
    langs
}

/// Один -J запрос (клиент web_embedded) → длительность и языки аудиодорожек.
/// Мультиязычные дубляжи YouTube видны только через web_embedded. Так и карточка,
/// и блок выбора дорожки появляются сразу вместе (без второго запроса).
pub async fn fetch_duration_and_tracks(url: &str) -> (Option<u64>, Vec<String>) {
    let args = vec![
        "-J".into(),
        "--no-playlist".into(),
        "--extractor-args".into(),
        YT_MULTI_AUDIO_CLIENT.into(),
        url.to_string(),
    ];
    let Ok(output) = run_ytdlp_output(args, "Failed to fetch info".to_string(), None).await else {
        return (None, Vec::new());
    };
    if !output.status.success() {
        return (None, Vec::new());
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return (None, Vec::new());
    };
    let duration = json["duration"].as_f64().map(|d| d as u64);
    (duration, parse_audio_langs(&json))
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
    // Берём первую непустую строку и игнорируем "NA" (yt-dlp печатает так для
    // отсутствующей длительности, напр. у прямых эфиров). Разбор по строкам,
    // а не по всему буферу, устойчив к \r\n и лишнему выводу на Windows.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != "NA")
        .and_then(|l| l.parse::<f64>().ok())
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

    let mut child = new_command(&yt_dlp())
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

    let status = wait_cancellable(&mut child).await?;

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

    let mut child = new_command(&yt_dlp())
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

    // pid берём заранее: в ветке отмены child уже перемещён в wait_with_output.
    let pid = child.id();
    if is_cancelled() {
        kill_group(pid);
        return Err(CANCEL_MSG.into());
    }
    let output = tokio::select! {
        out = child.wait_with_output() => out.map_err(|e| format!("{error_format}: {e}"))?,
        _ = CANCEL.notified() => {
            kill_group(pid);   // убить всю группу, а не только бутлоадер
            return Err(CANCEL_MSG.into());
        }
    };

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
    audio_lang: Option<String>,
) -> Result<DownloadResult, String> {
    begin_download();
    let out_dir = resolve_out_dir(&app, path);
    let app = Some(app);

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    if matches!(mode, Some(DownloadMode::Audio)) {
        return download_audio(&url, &out_dir, start, end, duration, audio_lang, app).await;
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

    let format = video_format_with_lang(quality.unwrap_or(Quality::Best), audio_lang.as_deref());
    // ВАЖНО: -o должен быть ОТНОСИТЕЛЬНЫМ. При абсолютном пути yt-dlp игнорирует
    // все --paths (и home, и temp) с предупреждением. Каталоги задаём через -P.
    let out_tmpl = format!("{filename}.%(ext)s");

    // Отдельный каталог для промежуточных файлов (-P temp:), как в fish-функции.
    let tmp_dir = out_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;

    let mut args: Vec<String> = vec![
        "-f".into(),
        format.into(),
        "--merge-output-format".into(),
        "mp4".into(),
        "--no-playlist".into(),
        "-P".into(),
        format!("home:{}", out_dir.to_string_lossy()),
        "-P".into(),
        format!("temp:{}", tmp_dir.to_string_lossy()),
    ];
    // Для конкретной аудиодорожки нужен клиент, раскрывающий дубляжи.
    if audio_lang.as_deref().is_some_and(|l| !l.is_empty()) {
        args.push("--extractor-args".into());
        args.push(YT_MULTI_AUDIO_CLIENT.into());
    }
    args.extend(network_args());
    args.push("-o".into());
    args.push(out_tmpl);
    args.push(url.clone());

    let status = run_ytdlp_status(args, "Failed to run yt-dlp".to_string(), app.clone()).await?;

    cleanup_temp(&out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

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

        let ffmpeg_status = run_ffmpeg(ffmpeg_args).await?;

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
    audio_lang: Option<String>,
    app: Option<AppHandle>,
) -> Result<DownloadResult, String> {
    let title = fetch_title(url).await?;
    let filename = sanitize_filename(&title);

    // Относительный -o + каталоги через -P (см. пояснение в download_video).
    let out_tmpl = format!("{filename}.%(ext)s");

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

    let tmp_dir = out_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;

    let mut args: Vec<String> = vec![
        "-f".into(),
        audio_format_with_lang(audio_lang.as_deref()),
        "-x".into(),
        "--audio-format".into(),
        "mp3".into(),
        "--audio-quality".into(),
        "0".into(),
        "--no-playlist".into(),
        "-P".into(),
        format!("home:{}", out_dir.to_string_lossy()),
        "-P".into(),
        format!("temp:{}", tmp_dir.to_string_lossy()),
    ];
    if audio_lang.as_deref().is_some_and(|l| !l.is_empty()) {
        args.push("--extractor-args".into());
        args.push(YT_MULTI_AUDIO_CLIENT.into());
    }
    args.extend(network_args());
    args.push("-o".into());
    args.push(out_tmpl);
    args.push("--print".into());
    args.push("after_move:filepath".into());
    args.push(url.to_string());

    let output = run_ytdlp_output(args, "yt-dlp error:".to_string(), app.clone()).await?;

    cleanup_temp(out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

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

        let ffmpeg_status = run_ffmpeg(ffmpeg_args).await?;

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
