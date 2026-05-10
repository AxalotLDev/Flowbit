mod functions;

use crate::functions::get_info::{get_twitch_info, get_youtube_info};
use crate::functions::twitch::download_twitch;
use crate::functions::valid::{is_twitch_url, is_youtube_url, validate_time_range};
use crate::functions::youtube::download_video;
use functions::twitch::TwitchDownloadState;
use functions::youtube::DownloadState;

use crate::functions::dependencies::install_dependencies;
use crate::functions::playlist::{download_playlist, get_playlist_info, is_playlist_url};
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
            is_playlist_url,
            get_playlist_info,
            download_playlist,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
