use crate::functions::dependencies::{ffmpeg, quickjs, yt_dlp};
use once_cell::sync::Lazy;
use regex::Regex;
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
/// Декодирует вывод yt-dlp/ffmpeg в строку. Обычно это UTF-8, но на Windows
/// yt-dlp пишет stdout/stderr в системной ANSI-кодировке (для русской локали —
/// cp1251/windows-1251), и кириллица бьётся в «ромбики» (U+FFFD) при чтении как
/// UTF-8. Пробуем UTF-8, при ошибке — windows-1251.
pub fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1251.decode(bytes).0.into_owned(),
    }
}

/// yt-dlp может решить, что вывод — терминал, поддерживающий ANSI-цвета (это
/// зависит от платформенной эвристики и ненадёжно, когда процесс запущен без
/// консоли, как на Windows с CREATE_NO_WINDOW). На этот случай подчищаем
/// управляющие последовательности из уже задекодированной строки, чтобы в
/// панели логов не оставалось "мусора" вида `\x1b[0;33m`.
static ANSI_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap());

fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    ANSI_RE.replace_all(s, "")
}

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

/// Ограничивает «жизнь» флага отмены рамками одной загрузки. Сбрасывает флаг
/// и при входе в загрузку, и при выходе из неё (любым путём — успех, ошибка,
/// отмена). Без сброса на выходе отменённая загрузка оставляла бы
/// CANCEL_REQUESTED = true, и последующие запросы метаданных (get_info →
/// fetch_duration_and_tracks → run_ytdlp_output) мгновенно падали бы с CANCEL_MSG,
/// из-за чего у следующего видео длительность не определялась (00:00:00).
pub struct DownloadGuard;
impl DownloadGuard {
    pub fn new() -> Self {
        CANCEL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        CANCEL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
    }
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
        "--compat-options",
        "filename-sanitization",
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

#[inline]
pub fn quality_to_format(q: Quality) -> &'static str {
    // Приоритет — универсально играбельные кодеки H.264 (avc1) + AAC (mp4a) в mp4:
    // их понимает любой плеер, включая VLC. YouTube по умолчанию отдаёт VP9/AV1 +
    // Opus, из-за чего VLC пишет «кодек не найден». VP9/AV1 берём только запасным
    // вариантом (например, для 4K, где H.264 нет).
    match q {
        Quality::Best => {
            "bv*[vcodec^=avc1]+ba[acodec^=mp4a]/bv*[ext=mp4]+ba[ext=m4a]/bv*+ba/b"
        }
        Quality::High => {
            "bv*[height<=1080][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=1080][ext=mp4]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]/b"
        }
        Quality::Medium => {
            "bv*[height<=720][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=720][ext=mp4]+ba[ext=m4a]/bv*[height<=720]+ba/b[height<=720]/b"
        }
        Quality::Low => {
            "bv*[height<=480][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=480][ext=mp4]+ba[ext=m4a]/bv*[height<=480]+ba/b[height<=480]/b"
        }
        Quality::Worst => "wv*[vcodec^=avc1]+wa[acodec^=mp4a]/wv*+wa/w",
    }
}

/// Клиент YouTube, раскрывающий мультиязычные аудиодорожки. Дефолтный клиент
/// отдаёт только оригинальную дорожку; web_embedded — все дубляжи. Указываем оба,
/// чтобы видео осталось в максимальном качестве (до 4K), а аудио — на всех языках.
const YT_MULTI_AUDIO_CLIENT: &str = "youtube:player_client=default,web_embedded";

/// Читает путь, записанный yt-dlp через `--print-to-file`. Обычно UTF-8, но на
/// всякий случай декодируем устойчиво (UTF-8 → cp1251), а не строгим read_to_string.
pub async fn read_printed_path(path_file: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path_file).await.ok()?;
    let content = decode_output(&bytes);
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .map(String::from)
}

fn clipped_path(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let ext = file.extension().unwrap_or_default().to_string_lossy();
    file.with_file_name(format!("{stem}_cut.{ext}"))
}

/// yt-dlp-фильтр по кодеку видео для нашего короткого имени. VP9 на YouTube
/// встречается и как `vp9`, и как `vp09.*` — покрываем оба регуляркой (~=).
fn vcodec_filter(codec: Option<&str>) -> Option<&'static str> {
    match codec {
        Some("h264") => Some("[vcodec^=avc1]"),
        Some("vp9") => Some("[vcodec~='^vp0?9']"),
        Some("av1") => Some("[vcodec^=av01]"),
        _ => None,
    }
}

fn acodec_filter(codec: Option<&str>) -> Option<&'static str> {
    match codec {
        Some("aac") => Some("[acodec^=mp4a]"),
        Some("opus") => Some("[acodec^=opus]"),
        _ => None,
    }
}

/// Контейнер (--merge-output-format) под выбор аудио. Opus официально не
/// поддерживается в mp4 — кладём в mkv (играет везде), иначе mp4.
pub fn merge_container(audio_codec: Option<&str>) -> &'static str {
    if audio_codec == Some("opus") {
        "mkv"
    } else {
        "mp4"
    }
}

/// Полный селектор формата для видео-режима: качество + кодек видео + кодек
/// аудио + язык дорожки. Строит список предпочтений от точного совпадения к
/// всё более общему (через `/`), чтобы загрузка не падала, когда точной
/// комбинации нет. Для «авто» (кодек не задан) предпочитаем совместимые
/// H.264 + AAC — их понимает любой плеер, включая VLC.
fn build_video_format(
    q: Quality,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_lang: Option<&str>,
) -> String {
    let (vbase, abase, cap) = match q {
        Quality::Best => ("bv*", "ba", ""),
        Quality::High => ("bv*", "ba", "[height<=1080]"),
        Quality::Medium => ("bv*", "ba", "[height<=720]"),
        Quality::Low => ("bv*", "ba", "[height<=480]"),
        Quality::Worst => ("wv*", "wa", ""),
    };
    let lang = audio_lang.filter(|l| !l.is_empty());
    let langf = lang.map(|l| format!("[language={l}]")).unwrap_or_default();

    // Для «авто» — совместимые кодеки; при явном выборе — заданный.
    let vf = vcodec_filter(video_codec).unwrap_or("[vcodec^=avc1]");
    let af = acodec_filter(audio_codec).unwrap_or("[acodec^=mp4a]");
    let a_explicit = acodec_filter(audio_codec).is_some();

    let v = |extra: &str| format!("{vbase}{cap}{extra}");
    let a = |extra: &str| format!("{abase}{langf}{extra}");

    let mut prefs: Vec<String> = Vec::new();
    // 1. точная комбинация (для авто — H.264 + AAC)
    prefs.push(format!("{}+{}", v(vf), a(af)));
    // 2. выбранный видеокодек + любое аудио (на нужном языке)
    prefs.push(format!("{}+{}", v(vf), a("")));
    // 3. если аудиокодек задан явно — любой видеокодек + нужное аудио
    if a_explicit {
        prefs.push(format!("{}+{}", v(""), a(af)));
    }
    // 4. любой видеокодек + любое аудио (на нужном языке)
    prefs.push(format!("{}+{}", v(""), a("")));
    // 5. если был язык — те же варианты без языка (дорожки может не быть)
    if lang.is_some() {
        prefs.push(format!("{}+{}", v(vf), abase));
        prefs.push(format!("{}+{}", v(""), abase));
    }
    // 6. финальный общий fallback
    prefs.push(format!("b{cap}"));
    prefs.push("b".into());

    prefs.dedup();
    prefs.join("/")
}

/// Селектор формата для режима «только аудио»: выбор исходной дорожки по языку
/// и кодеку (выходной контейнер задаётся отдельно через --audio-format).
fn build_audio_format(audio_codec: Option<&str>, audio_lang: Option<&str>) -> String {
    let lang = audio_lang.filter(|l| !l.is_empty());
    let langf = lang.map(|l| format!("[language={l}]")).unwrap_or_default();
    let af = acodec_filter(audio_codec);

    let mut prefs: Vec<String> = Vec::new();
    if let Some(f) = af {
        prefs.push(format!("ba{langf}{f}"));
    }
    prefs.push(format!("ba{langf}"));
    if lang.is_some() {
        if let Some(f) = af {
            prefs.push(format!("ba{f}"));
        }
    }
    prefs.push("ba".into());

    prefs.dedup();
    prefs.join("/")
}

/// Порядок отображения кодеков — фиксированный, чтобы UI был стабилен.
const VCODEC_ORDER: [&str; 3] = ["h264", "vp9", "av1"];
const ACODEC_ORDER: [&str; 2] = ["aac", "opus"];

fn canon_vcodec(vcodec: &str) -> Option<&'static str> {
    if vcodec.starts_with("avc1") || vcodec.starts_with("avc3") || vcodec.starts_with("h264") {
        Some("h264")
    } else if vcodec.starts_with("vp9") || vcodec.starts_with("vp09") {
        Some("vp9")
    } else if vcodec.starts_with("av01") {
        Some("av1")
    } else {
        None
    }
}

fn canon_acodec(acodec: &str) -> Option<&'static str> {
    if acodec.starts_with("mp4a") || acodec.starts_with("aac") {
        Some("aac")
    } else if acodec.starts_with("opus") {
        Some("opus")
    } else {
        None
    }
}

pub fn parse_video_codecs(json: &serde_json::Value) -> Vec<String> {
    let mut found = std::collections::HashSet::new();
    if let Some(formats) = json["formats"].as_array() {
        for f in formats {
            if let Some(v) = f["vcodec"].as_str() {
                if v != "none" {
                    if let Some(c) = canon_vcodec(v) {
                        found.insert(c);
                    }
                }
            }
        }
    }
    VCODEC_ORDER
        .iter()
        .filter(|c| found.contains(**c))
        .map(|c| c.to_string())
        .collect()
}

pub fn parse_audio_codecs(json: &serde_json::Value) -> Vec<String> {
    let mut found = std::collections::HashSet::new();
    if let Some(formats) = json["formats"].as_array() {
        for f in formats {
            let is_audio = f["acodec"].as_str().is_some_and(|a| a != "none");
            if is_audio {
                if let Some(a) = f["acodec"].as_str().and_then(canon_acodec) {
                    found.insert(a);
                }
            }
        }
    }
    ACODEC_ORDER
        .iter()
        .filter(|c| found.contains(**c))
        .map(|c| c.to_string())
        .collect()
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
/// Метаданные видео из одного вызова `yt-dlp -J`. Используются как запасной
/// источник, когда oembed недоступен (401/404 у видео с запретом встраивания,
/// возрастным/региональным ограничением).
#[derive(Default)]
pub struct YtMeta {
    pub duration: Option<u64>,
    pub audio_tracks: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub thumbnail: Option<String>,
}

/// Один вызов `yt-dlp -J`. `multi_audio` включает web_embedded-клиент, который
/// раскрывает мультиязычные дубляжи, но иногда отдаёт неполный ответ (без
/// duration/формата) — поэтому есть откат на дефолтный клиент.
async fn run_meta_json(url: &str, multi_audio: bool) -> Option<serde_json::Value> {
    let mut args = vec!["-J".to_string(), "--no-playlist".to_string()];
    if multi_audio {
        args.push("--extractor-args".into());
        args.push(YT_MULTI_AUDIO_CLIENT.into());
    }
    args.push(url.to_string());
    let output = run_ytdlp_output(args, "Failed to fetch info".to_string(), None)
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json = serde_json::from_str::<serde_json::Value>(&decode_output(&output.stdout)).ok()?;
    // При неудачной экстракции (в т.ч. капча «not a bot») yt-dlp печатает "null"
    // и выходит с кодом 0. Это не метаданные — считаем провалом, чтобы сработал
    // откат на другой клиент, а не тихо получить duration = null (00:00:00).
    if !json.is_object() {
        return None;
    }
    Some(json)
}

/// Пытается получить валидный JSON метаданных, устойчиво к капче «not a bot».
/// Сначала web_embedded (раскрывает дубляжи); если пусто/капча — дефолтный
/// клиент с несколькими ретраями с задержкой: капча обычно снимается через пару
/// секунд, поэтому один отказ ещё не значит, что видео недоступно.
async fn fetch_meta_json_resilient(url: &str) -> Option<serde_json::Value> {
    if let Some(j) = run_meta_json(url, true).await {
        return Some(j);
    }
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        }
        if let Some(j) = run_meta_json(url, false).await {
            return Some(j);
        }
    }
    None
}

pub async fn fetch_yt_meta(url: &str, app: Option<AppHandle>) -> YtMeta {
    let json = fetch_meta_json_resilient(url).await;
    let Some(json) = json else {
        let duration = fetch_duration(url).await;
        if duration.is_none() {
            emit_log(
                &app,
                "[flowbit] Не удалось получить данные видео (возможно, ограничение YouTube «not a bot»). Длительность неизвестна.",
            );
        }
        return YtMeta {
            duration,
            ..Default::default()
        };
    };
    let s = |k: &str| json[k].as_str().filter(|v| !v.is_empty()).map(String::from);
    let mut duration = json["duration"].as_f64().map(|d| d as u64);
    if duration.is_none() {
        duration = fetch_duration(url).await;
    }
    YtMeta {
        duration,
        audio_tracks: parse_audio_langs(&json),
        video_codecs: parse_video_codecs(&json),
        audio_codecs: parse_audio_codecs(&json),
        title: s("title").or_else(|| s("fulltitle")),
        author: s("uploader").or_else(|| s("channel")).or_else(|| s("uploader_id")),
        thumbnail: s("thumbnail"),
    }
}

pub fn parse_time_to_secs(t: &str) -> Option<u64> {
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

/// Читает вывод построчно по байтам и декодирует каждую строку через
/// decode_output. В отличие от tokio `.lines()` (только UTF-8, обрывается на
/// первой не-UTF-8 строке), это не теряет логи и корректно показывает кириллицу
/// из cp1251-вывода yt-dlp на Windows.
async fn stream_lines(reader: impl tokio::io::AsyncRead + Unpin, app: Option<AppHandle>) {
    let mut segments = BufReader::new(reader).split(b'\n');
    while let Ok(Some(seg)) = segments.next_segment().await {
        let mut line = decode_output(&seg);
        line.retain(|c| c != '\r');
        if line.contains('\x1b') {
            line = strip_ansi(&line).into_owned();
        }
        emit_log(&app, &line);
    }
}

/// Общие флаги для каждого запуска yt-dlp: расположение ffmpeg, JS-рантайм
/// для чтения n-sig и `--color never` — платформенное определение поддержки
/// ANSI-цвета ненадёжно, когда процесс запущен без консоли (Windows,
/// CREATE_NO_WINDOW), и без явного отключения в панель логов иногда попадают
/// сырые управляющие последовательности.
fn default_ytdlp_args() -> Vec<String> {
    vec![
        "--ffmpeg-location".into(),
        ffmpeg().into(),
        "--js-runtimes".into(),
        format!("quickjs:{}", quickjs()),
        "--color".into(),
        "never".into(),
    ]
}

pub async fn run_ytdlp_status(
    args: Vec<String>,
    error_format: String,
    app: Option<AppHandle>,
) -> Result<std::process::ExitStatus, String> {
    let default_args = default_ytdlp_args();

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
            stream_lines(s, app_out).await;
        }
    });

    let h_err = tokio::spawn(async move {
        if let Some(s) = stderr {
            stream_lines(s, app_err).await;
        }
    });

    let status = wait_cancellable(&mut child).await?;

    let _ = h_out.await;
    let _ = h_err.await;

    Ok(status)
}

pub async fn run_ytdlp_output(
    args: Vec<String>,
    error_format: String,
    app: Option<AppHandle>,
) -> Result<std::process::Output, String> {
    let default_args = default_ytdlp_args();

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
            stream_lines(s, app_err).await;
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
    video_codec: Option<String>,
    audio_codec: Option<String>,
) -> Result<DownloadResult, String> {
    let _guard = DownloadGuard::new();
    let out_dir = resolve_out_dir(&app, path);
    let app = Some(app);

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    if matches!(mode, Some(DownloadMode::Audio)) {
        return download_audio(
            &url, &out_dir, start, end, duration, audio_lang, audio_codec, app,
        )
        .await;
    }

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

    let format = build_video_format(
        quality.unwrap_or(Quality::Best),
        video_codec.as_deref().filter(|c| !c.is_empty()),
        audio_codec.as_deref().filter(|c| !c.is_empty()),
        audio_lang.as_deref(),
    );
    let container = merge_container(audio_codec.as_deref().filter(|c| !c.is_empty()));

    // Отдельный каталог для промежуточных файлов (-P temp:), как в fish-функции.
    let tmp_dir = out_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    // yt-dlp пишет сюда реальный путь в UTF-8. Имя файла НЕ строим сами из title:
    // на Windows stdout yt-dlp бывает в cp1251, и кириллица бьётся в «ромбики».
    // %(title)s даёт yt-dlp писать файл с корректным Unicode-именем, а путь
    // читаем из файла (--print-to-file всегда UTF-8), а не из stdout.
    let path_file = tmp_dir.join("__filepath.txt");
    let _ = tokio::fs::remove_file(&path_file).await;

    let mut args: Vec<String> = vec![
        "-f".into(),
        format,
        "--merge-output-format".into(),
        container.into(),
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
    args.push("%(title)s.%(ext)s".into());
    args.push("--print-to-file".into());
    args.push("after_move:filepath".into());
    args.push(path_file.to_string_lossy().into_owned());
    args.push(url.clone());

    let status = run_ytdlp_status(args, "Failed to run yt-dlp".to_string(), app.clone()).await?;

    // Читаем путь ДО удаления tmp_dir.
    let real_path = read_printed_path(&path_file).await;

    cleanup_temp(&out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    if !status.success() {
        return Err("Video download failed".into());
    }

    let out_file = PathBuf::from(real_path.ok_or("Cannot resolve output file path")?);

    if need_section {
        let clipped_file = clipped_path(&out_file);

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            out_file.to_string_lossy().into_owned(),
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

        let _ = tokio::fs::remove_file(&out_file).await;
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
    audio_codec: Option<String>,
    app: Option<AppHandle>,
) -> Result<DownloadResult, String> {
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
    // Имя файла отдаём yt-dlp (%(title)s), путь читаем из UTF-8 файла — см. download_video.
    let path_file = tmp_dir.join("__filepath.txt");
    let _ = tokio::fs::remove_file(&path_file).await;

    let ac = audio_codec.as_deref().filter(|c| !c.is_empty());
    // Выходной формат аудио: opus/aac сохраняем как есть (без потерь на
    // перекодировании), для «авто» — mp3 (играет везде).
    let out_audio_format = match ac {
        Some("opus") => "opus",
        Some("aac") => "m4a",
        _ => "mp3",
    };
    let mut args: Vec<String> = vec![
        "-f".into(),
        build_audio_format(ac, audio_lang.as_deref()),
        "-x".into(),
        "--audio-format".into(),
        out_audio_format.into(),
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
    args.push("%(title)s.%(ext)s".into());
    args.push("--print-to-file".into());
    args.push("after_move:filepath".into());
    args.push(path_file.to_string_lossy().into_owned());
    args.push(url.to_string());

    let output = run_ytdlp_output(args, "yt-dlp error:".to_string(), app.clone()).await?;

    let real_path = read_printed_path(&path_file).await;

    cleanup_temp(out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    if !output.status.success() {
        return Err("Audio download failed".into());
    }

    let out_file = PathBuf::from(real_path.ok_or("Cannot resolve output file path from yt-dlp")?);

    if need_section {
        let clipped_file = clipped_path(&out_file);

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            out_file.to_string_lossy().into_owned(),
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

        let _ = tokio::fs::remove_file(&out_file).await;
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

#[cfg(test)]
mod decode_tests {
    use super::decode_output;
    #[test]
    fn utf8_passthrough() {
        assert_eq!(decode_output("Привет.mp4".as_bytes()), "Привет.mp4");
    }
    #[test]
    fn cp1251_fallback() {
        // "Привет" в cp1251
        let cp1251 = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(decode_output(&cp1251), "Привет");
    }
    #[test]
    fn ascii_ok() {
        assert_eq!(decode_output(b"[download] 50%"), "[download] 50%");
    }
}
