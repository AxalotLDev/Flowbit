mod functions;

use crate::functions::download::download_video;
use crate::functions::get_info::{get_twitch_info, get_youtube_info};
use crate::functions::twitch::download_twitch;
use crate::functions::valid::{is_twitch_url, is_youtube_url, validate_time_range};
use functions::download::DownloadState;
use functions::twitch::TwitchDownloadState;

use crate::functions::dependencies::install_dependencies;
use tauri::Manager;

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
            is_youtube_url,
            is_twitch_url,
            validate_time_range,
            get_youtube_info,
            get_twitch_info,
            download_video,
            download_twitch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
