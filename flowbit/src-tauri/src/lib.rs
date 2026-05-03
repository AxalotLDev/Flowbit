mod functions;
use std::path::PathBuf;
use yt_dlp::Downloader;
use functions::download::DownloadState;
use functions::twitch::TwitchDownloadState;

#[tauri::command]
async fn install_dependencies() -> Result<(), String> {
    Downloader::with_new_binaries(PathBuf::from("libs"), PathBuf::from("output"))
        .await
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| {
            tauri::async_runtime::spawn(async {
                if let Err(e) = install_dependencies().await {
                    eprintln!("Failed to install dependencies: {}", e);
                }
            });
            Ok(())
        })
        .manage(DownloadState::new())
        .manage(TwitchDownloadState::new())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            functions::valid::is_youtube_url,
            functions::get_info::get_youtube_info,
            functions::download::download_video,
            functions::twitch::is_twitch_url,
            functions::twitch::get_twitch_info,
            functions::twitch::download_twitch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}