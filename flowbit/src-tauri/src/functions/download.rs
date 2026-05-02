use yt_dlp::Downloader;
use yt_dlp::client::deps::Libraries;
use yt_dlp::model::selector::{VideoQuality, AudioQuality, VideoCodecPreference};
use std::path::{PathBuf, Path};
use tauri::{AppHandle, Emitter, State};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

#[derive(Serialize, Clone)]
pub struct DownloadProgress {
    pub percent: f64,
    pub speed: String,
}

#[derive(Serialize, Clone)]
pub struct DownloadError {
    pub message: String,
    pub error_type: String,
    pub technical_details: Option<String>,
}

// Состояние для хранения активных загрузок
pub struct DownloadState {
    active_downloads: Arc<Mutex<HashMap<String, bool>>>, // url -> is_canceled
}

impl DownloadState {
    pub fn new() -> Self {
        Self {
            active_downloads: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug)]
enum DownloadErrorType {
    NetworkError,
    FileSystemError,
    VideoUnavailable,
    FormatNotSupported,
    DownloadCanceled,
    ParseError,
    FfmpegError,
    Unknown,
}

impl std::fmt::Display for DownloadErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadErrorType::NetworkError => write!(f, "NETWORK_ERROR"),
            DownloadErrorType::FileSystemError => write!(f, "FILESYSTEM_ERROR"),
            DownloadErrorType::VideoUnavailable => write!(f, "VIDEO_UNAVAILABLE"),
            DownloadErrorType::FormatNotSupported => write!(f, "FORMAT_NOT_SUPPORTED"),
            DownloadErrorType::DownloadCanceled => write!(f, "DOWNLOAD_CANCELED"),
            DownloadErrorType::ParseError => write!(f, "PARSE_ERROR"),
            DownloadErrorType::FfmpegError => write!(f, "FFMPEG_ERROR"),
            DownloadErrorType::Unknown => write!(f, "UNKNOWN_ERROR"),
        }
    }
}

fn classify_error(error: &str) -> (DownloadErrorType, String) {
    let error_lower = error.to_lowercase();
    
    if error_lower.contains("canceled") || error_lower.contains("cancelled") {
        return (DownloadErrorType::DownloadCanceled, 
                "Download was canceled by user".to_string());
    }
    
    if error_lower.contains("network") || error_lower.contains("connection") || 
       error_lower.contains("timeout") || error_lower.contains("dns") ||
       error_lower.contains("ssl") || error_lower.contains("http") {
        return (DownloadErrorType::NetworkError, 
                format!("Network error occurred: Check your internet connection. {}", 
                       get_network_error_details(error)));
    }
    
    if error_lower.contains("permission") || error_lower.contains("access denied") ||
       error_lower.contains("disk full") || error_lower.contains("no space") ||
       error_lower.contains("read-only") {
        return (DownloadErrorType::FileSystemError,
                format!("File system error: {}", get_filesystem_error_details(error)));
    }
    
    if error_lower.contains("video unavailable") || error_lower.contains("private video") ||
       error_lower.contains("deleted") || error_lower.contains("not found") ||
       error_lower.contains("removed") || error_lower.contains("geoblocked") ||
       error_lower.contains("age restricted") {
        return (DownloadErrorType::VideoUnavailable,
                format!("Video is unavailable: {}", get_video_unavailable_details(error)));
    }
    
    if error_lower.contains("format") || error_lower.contains("codec") ||
       error_lower.contains("resolution") || error_lower.contains("bitrate") {
        return (DownloadErrorType::FormatNotSupported,
                format!("Format not supported: {}", get_format_error_details(error)));
    }
    
    if error_lower.contains("parse") || error_lower.contains("json") ||
       error_lower.contains("xml") || error_lower.contains("extract") {
        return (DownloadErrorType::ParseError,
                format!("Failed to parse video information: {}", error));
    }
    
    if error_lower.contains("ffmpeg") || error_lower.contains("merge") ||
       error_lower.contains("convert") || error_lower.contains("encode") {
        return (DownloadErrorType::FfmpegError,
                format!("Video processing error: {}. Try reinstalling ffmpeg.", error));
    }
    
    (DownloadErrorType::Unknown, format!("An unexpected error occurred: {}", error))
}

fn get_network_error_details(error: &str) -> String {
    let error_lower = error.to_lowercase();
    
    if error_lower.contains("timeout") {
        "The connection timed out. Try again later.".to_string()
    } else if error_lower.contains("ssl") || error_lower.contains("tls") {
        "SSL/TLS certificate verification failed.".to_string()
    } else if error_lower.contains("dns") {
        "DNS resolution failed. Check your DNS settings.".to_string()
    } else if error_lower.contains("403") {
        "Access forbidden (HTTP 403). The server may be blocking the request.".to_string()
    } else if error_lower.contains("404") {
        "Resource not found (HTTP 404). The URL might be incorrect.".to_string()
    } else if error_lower.contains("429") {
        "Too many requests (HTTP 429). Try again later.".to_string()
    } else if error_lower.contains("5") && error_lower.contains("xx") {
        "Server error. The video platform might be experiencing issues.".to_string()
    } else {
        "Please check your internet connection and firewall settings.".to_string()
    }
}

fn get_filesystem_error_details(error: &str) -> String {
    let error_lower = error.to_lowercase();
    
    if error_lower.contains("permission") || error_lower.contains("access denied") {
        "No write permission for the download directory. Choose a different folder.".to_string()
    } else if error_lower.contains("disk full") || error_lower.contains("no space") {
        "Not enough disk space. Free up space and try again.".to_string()
    } else if error_lower.contains("read-only") {
        "The selected directory is read-only. Choose a writable location.".to_string()
    } else {
        "Could not write to the download location.".to_string()
    }
}

fn get_video_unavailable_details(error: &str) -> String {
    let error_lower = error.to_lowercase();
    
    if error_lower.contains("private") {
        "This video is private and cannot be downloaded without authentication.".to_string()
    } else if error_lower.contains("deleted") || error_lower.contains("removed") {
        "This video has been deleted or removed from the platform.".to_string()
    } else if error_lower.contains("geoblocked") || error_lower.contains("region") {
        "This video is not available in your region. Try using a VPN.".to_string()
    } else if error_lower.contains("age restricted") || error_lower.contains("age-restricted") {
        "This video requires age verification.".to_string()
    } else if error_lower.contains("not found") {
        "The video was not found. The URL might be incorrect.".to_string()
    } else {
        "The video is unavailable for an unknown reason.".to_string()
    }
}

fn get_format_error_details(error: &str) -> String {
    let error_lower = error.to_lowercase();
    
    if error_lower.contains("resolution") {
        "The requested video resolution is not available.".to_string()
    } else if error_lower.contains("codec") {
        "The video codec is not supported. Try a different format.".to_string()
    } else if error_lower.contains("bitrate") {
        "The requested bitrate is not available.".to_string()
    } else {
        "The requested format is not available for this video.".to_string()
    }
}

fn get_default_downloads_folder() -> String {
    // Сначала проверяем HOME (Linux/Mac)
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(&home).join("Downloads");
        if path.exists() {
            return path.to_string_lossy().to_string();
        }
    }
    
    // Затем проверяем USERPROFILE (Windows)
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let path = PathBuf::from(&userprofile).join("Downloads");
        if path.exists() {
            return path.to_string_lossy().to_string();
        }
    }
    
    // Запасной вариант - текущая директория
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

#[tauri::command]
pub async fn download_video(
    app: AppHandle,
    state: State<'_, DownloadState>,
    url: String,
    path: Option<String>,
) -> Result<String, String> {
    // Нормализуем путь для текущей ОС
    let download_path = if let Some(p) = path {
        PathBuf::from(&p)
    } else {
        PathBuf::from(get_default_downloads_folder())
    };
    
    // Убеждаемся, что директория существует
    if let Err(e) = tokio::fs::create_dir_all(&download_path).await {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            let error_msg = format!(
                "Cannot create download directory '{}': {}. Please check permissions.",
                download_path.display(),
                e
            );
            emit_error(&app, &error_msg, "FILESYSTEM_ERROR");
            return Err(error_msg);
        }
    }
    
    let libraries = Libraries::new(
        PathBuf::from("libs/yt-dlp"),
        PathBuf::from("libs/ffmpeg"),
    );

    let downloader = match Downloader::builder(libraries, &download_path)
        .build()
        .await
    {
        Ok(d) => d,
        Err(e) => {
            let error_msg = format!("Failed to initialize downloader: {}", e);
            emit_error(&app, &error_msg, "UNKNOWN_ERROR");
            return Err(error_msg);
        }
    };

    // Добавляем URL в активные загрузки
    {
        let mut downloads = state.active_downloads.lock().unwrap();
        downloads.insert(url.clone(), false);
    }

    let _ = app.emit("download-progress", DownloadProgress {
        percent: 10.0,
        speed: "Fetching video info...".to_string(),
    });

    let video = match downloader
        .fetch_video_infos(url.clone())
        .await
    {
        Ok(v) => v,
        Err(e) => {
            let error_str = e.to_string();
            let (error_type, message) = classify_error(&error_str);
            
            emit_error(&app, &message, &error_type.to_string());
            
            // Удаляем из активных загрузок
            let mut downloads = state.active_downloads.lock().unwrap();
            downloads.remove(&url);
            
            return Err(format!("{}|{}", error_type, message));
        }
    };
    
    // Проверяем отмену
    {
        let downloads = state.active_downloads.lock().unwrap();
        if downloads.get(&url).copied().unwrap_or(false) {
            let _ = app.emit("download-progress", DownloadProgress {
                percent: 0.0,
                speed: "Canceled".to_string(),
            });
            
            let error_msg = "Download was canceled by user".to_string();
            emit_error(&app, &error_msg, "DOWNLOAD_CANCELED");
            
            let mut downloads = state.active_downloads.lock().unwrap();
            downloads.remove(&url);
            
            return Err(format!("DOWNLOAD_CANCELED|{}", error_msg));
        }
    }
    
    let filename = sanitize_filename(&video.title);
    let full_filename = format!("{}.mp4", filename);
    
    // Создаем полный путь к файлу
    let output_path = download_path.join(&full_filename);
    
    // Проверяем, не существует ли уже файл
    if output_path.exists() {
        let error_msg = format!(
            "File '{}' already exists. Please choose a different location or rename the file.",
            output_path.display()
        );
        emit_error(&app, &error_msg, "FILESYSTEM_ERROR");
        
        let mut downloads = state.active_downloads.lock().unwrap();
        downloads.remove(&url);
        
        return Err(format!("FILESYSTEM_ERROR|{}", error_msg));
    }

    let _ = app.emit("download-progress", DownloadProgress {
        percent: 30.0,
        speed: "Downloading...".to_string(),
    });

    // Проверяем отмену перед скачиванием
    {
        let downloads = state.active_downloads.lock().unwrap();
        if downloads.get(&url).copied().unwrap_or(false) {
            let _ = app.emit("download-progress", DownloadProgress {
                percent: 0.0,
                speed: "Canceled".to_string(),
            });
            
            let error_msg = "Download was canceled by user".to_string();
            emit_error(&app, &error_msg, "DOWNLOAD_CANCELED");
            
            let mut downloads = state.active_downloads.lock().unwrap();
            downloads.remove(&url);
            
            return Err(format!("DOWNLOAD_CANCELED|{}", error_msg));
        }
    }

    // Используем Fluent API для КАЧЕСТВА
    let result = downloader
        .download(&video, &full_filename)
        .video_quality(VideoQuality::Best)
        .video_codec(VideoCodecPreference::Any)
        .audio_quality(AudioQuality::Best)
        .execute()
        .await;

    // Удаляем из активных и очищаем временные файлы
    {
        let mut downloads = state.active_downloads.lock().unwrap();
        downloads.remove(&url);
    }
    cleanup_temp_files(&download_path).await;

    match result {
        Ok(video_path) => {
            // Проверяем, что файл действительно существует и не пустой
            match tokio::fs::metadata(&video_path).await {
                Ok(metadata) => {
                    if metadata.len() == 0 {
                        let error_msg = "Downloaded file is empty. The download may have failed silently.".to_string();
                        emit_error(&app, &error_msg, "FILESYSTEM_ERROR");
                        return Err(format!("FILESYSTEM_ERROR|{}", error_msg));
                    }
                    
                    let _ = app.emit("download-progress", DownloadProgress {
                        percent: 100.0,
                        speed: format!("Completed ({:.2} MB)", metadata.len() as f64 / 1_048_576.0),
                    });
                    
                    // Возвращаем нормализованный путь
                    Ok(video_path.to_string_lossy().to_string())
                }
                Err(e) => {
                    let error_msg = format!(
                        "Download seemed successful but cannot access the file '{}': {}",
                        video_path.display(),
                        e
                    );
                    emit_error(&app, &error_msg, "FILESYSTEM_ERROR");
                    Err(format!("FILESYSTEM_ERROR|{}", error_msg))
                }
            }
        }
        Err(e) => {
            let error_str = e.to_string();
            let (error_type, message) = classify_error(&error_str);
            
            emit_error(&app, &message, &error_type.to_string());
            
            // Пытаемся удалить недокачанный файл
            if output_path.exists() {
                let _ = tokio::fs::remove_file(&output_path).await;
            }
            
            Err(format!("{}|{}", error_type, message))
        }
    }
}

fn emit_error(app: &AppHandle, message: &str, error_type: &str) {
    let error = DownloadError {
        message: message.to_string(),
        error_type: error_type.to_string(),
        technical_details: None,
    };
    let _ = app.emit("download-error", error);
}

#[tauri::command]
pub async fn cancel_download(
    state: State<'_, DownloadState>,
    url: String,
) -> Result<String, String> {
    let mut downloads = state.active_downloads.lock().unwrap();
    
    if let Some(is_canceled) = downloads.get_mut(&url) {
        *is_canceled = true;
        Ok("Download cancellation requested".to_string())
    } else {
        Err("No active download found for this URL".to_string())
    }
}

fn sanitize_filename(filename: &str) -> String {
    // Заменяем недопустимые символы в именах файлов для всех ОС
    let sanitized: String = filename
        .chars()
        .map(|c| match c {
            // Недопустимые символы в Windows и Linux
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            _ => c,
        })
        .collect();
    
    let sanitized = sanitized.trim().to_string();
    
    // Убеждаемся, что имя файла не пустое
    if sanitized.is_empty() {
        "video".to_string()
    } else {
        // Ограничиваем длину имени файла (для совместимости с разными ФС)
        if sanitized.len() > 200 {
            sanitized[..200].to_string()
        } else {
            sanitized
        }
    }
}

async fn cleanup_temp_files(directory: &Path) {
    if let Ok(mut entries) = tokio::fs::read_dir(directory).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(file_name) = entry.file_name().to_str() {
                if file_name.starts_with("temp_") || 
                   file_name.ends_with(".part") || 
                   file_name.ends_with(".ytdl") ||
                   file_name.ends_with(".tmp") ||
                   file_name.ends_with(".frag") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
        }
    }
}