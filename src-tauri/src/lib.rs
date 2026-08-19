pub mod functions;

use crate::functions::get_info::{get_twitch_info, get_youtube_info};
use crate::functions::twitch::download_twitch;
use crate::functions::valid::{is_twitch_url, is_youtube_url, validate_time_range};
use crate::functions::youtube::{
    cancel_download, download_video, update_ytdlp, ytdlp_self_update,
};
use functions::twitch::TwitchDownloadState;
use functions::youtube::DownloadState;

use crate::functions::dependencies::{deps_ready, install_dependencies};
use crate::functions::playlist::{download_playlist, get_playlist_info, is_playlist_url};
use crate::functions::preview::download_preview;
use tauri::Emitter;

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // Downloading the binaries does NOT block the window from appearing:
            // the frontend shows a loading screen and waits for "deps-ready"/"deps-error".
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = install_dependencies(&handle).await {
                    eprintln!("Failed to install dependencies: {e}");
                    let _ = handle.emit("deps-error", e);
                    return;
                }
                let _ = handle.emit("deps-ready", ());

                // Check for yt-dlp updates AFTER install, so we don't replace the
                // binary during the initial download / info requests.
                match ytdlp_self_update(Some(handle.clone())).await {
                    Ok(true) => {}
                    Ok(false) => eprintln!("yt-dlp update check: non-zero exit"),
                    Err(e) => eprintln!("yt-dlp update check failed: {e}"),
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
            download_preview,
            download_twitch,
            is_playlist_url,
            get_playlist_info,
            download_playlist,
            update_ytdlp,
            cancel_download,
            deps_ready,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
