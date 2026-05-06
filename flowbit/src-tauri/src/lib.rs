mod functions;

use functions::download::DownloadState;
use functions::twitch::TwitchDownloadState;
use functions::download_quickjs::download_quickjs;

use std::path::PathBuf;
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use yt_dlp::Downloader;
static LIBS_PATH: OnceLock<String> = OnceLock::new();

pub fn init_libs_dir_path(path: &PathBuf) {
    LIBS_PATH.set(path.to_string_lossy().to_string()).ok();
}
#[tauri::command]
async fn install_dependencies(app: &AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    let libs_dir = data_dir.join("libs");
    let output_dir = data_dir.join("output");

    std::fs::create_dir_all(&libs_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;

    init_libs_dir_path(&libs_dir);

    Downloader::with_new_binaries(libs_dir.clone(), output_dir)
        .await
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;

    download_quickjs(&libs_dir).await?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let _path = app
                .path()
                .app_data_dir()
                .expect("no app data dir")
                .join("libs")
                .join(if cfg!(windows) {
                    "yt-dlp.exe"
                } else {
                    "yt-dlp"
                });

            tauri::async_runtime::block_on(async {
                if let Err(e) = install_dependencies(app.handle()).await {
                    eprintln!("Failed: {}", e);
                }
            });
            Ok(())
        })
        .manage(DownloadState::new())
        .manage(TwitchDownloadState::new())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            functions::valid::is_youtube_url,
            functions::valid::is_twitch_url,
            functions::valid::validate_time_range,
            functions::get_info::get_youtube_info,
            functions::get_info::get_twitch_info,
            functions::download::download_video,
            functions::twitch::download_twitch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
