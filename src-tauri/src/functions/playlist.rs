use crate::functions::youtube::{
    cleanup_temp, file_mb, network_args, quality_to_format, resolve_out_dir, run_ytdlp_output,
    run_ytdlp_status, DownloadMode, Quality,
};
use serde::Serialize;
use std::path::PathBuf;
use tauri::AppHandle;

#[derive(Serialize, Clone)]
pub struct PlaylistInfo {
    pub title: String,
    pub uploader: String,
    pub count: u64,
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Serialize, Clone)]
pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub duration: Option<u64>,
    pub url: String,
}

#[derive(Serialize, Clone)]
pub struct PlaylistDownloadResult {
    pub dir: String,
    pub downloaded: u64,
    pub total: u64,
    pub total_size_mb: f64,
}

#[tauri::command]
pub fn is_playlist_url(url: String) -> bool {
    let u = url.trim();

    // list= without a specific video. A link to a single video (/watch?v=...
    // or youtu.be/<id>) carrying &list=... (radio, "watch later") counts as a
    // single video, not a playlist. A bare /playlist path with no list= param
    // isn't a resolvable playlist either.
    let points_to_single_video =
        (u.contains("watch") && u.contains("v=")) || u.contains("youtu.be");
    let is_yt_playlist =
        u.contains("youtube.com") && u.contains("list=") && !points_to_single_video;

    let is_twitch_playlist = is_twitch_videos_page(u);

    is_yt_playlist || is_twitch_playlist
}

/// True for a Twitch channel's videos listing (`twitch.tv/<channel>/videos`),
/// which is a playlist. A single VOD link (`twitch.tv/videos/<id>`) merely
/// contains "/videos" as a substring too, so this checks path segments
/// instead of using `.contains("/videos")`.
fn is_twitch_videos_page(u: &str) -> bool {
    let Some(idx) = u.find("twitch.tv/") else {
        return false;
    };
    let rest = &u[idx + "twitch.tv/".len()..];
    let segments: Vec<&str> = rest
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    matches!(segments.as_slice(), [channel, "videos"] if *channel != "videos")
}

#[tauri::command]
pub async fn get_playlist_info(url: String) -> Result<PlaylistInfo, String> {
    let args: Vec<String> = vec![
        "--flat-playlist".into(),
        "--print".into(),
        "%(playlist_title)s\t%(playlist_uploader)s\t%(playlist_count)s\t%(id)s\t%(title)s\t%(duration)s\t%(webpage_url)s".into(),
        "--no-warnings".into(),
        url.clone(),
    ];

    let output = run_ytdlp_output(args, "Failed to fetch playlist info".into(), None).await?;

    if !output.status.success() {
        return Err("yt-dlp returned an error for this playlist URL".into());
    }

    let text = crate::functions::youtube::decode_output(&output.stdout);
    let mut lines = text.lines();

    let first = lines.next().ok_or("Empty playlist response")?;
    let cols: Vec<&str> = first.splitn(7, '\t').collect();
    if cols.len() < 7 {
        return Err("Unexpected yt-dlp output format".into());
    }

    let playlist_title = cols[0].to_string();
    let uploader = cols[1].to_string();
    let count: u64 = cols[2].parse().unwrap_or(0);

    let mut entries = vec![make_entry(cols[3], cols[4], cols[5], cols[6])];

    for line in lines {
        let c: Vec<&str> = line.splitn(7, '\t').collect();
        if c.len() >= 7 {
            entries.push(make_entry(c[3], c[4], c[5], c[6]));
        }
    }

    let total = if count == 0 { entries.len() as u64 } else { count };

    Ok(PlaylistInfo {
        title: playlist_title,
        uploader,
        count: total,
        entries,
    })
}

fn make_entry(id: &str, title: &str, duration: &str, url: &str) -> PlaylistEntry {
    PlaylistEntry {
        id: id.to_string(),
        title: title.to_string(),
        duration: duration.parse::<f64>().ok().map(|d| d as u64),
        url: url.to_string(),
    }
}

#[tauri::command]
pub async fn download_playlist(
    app: AppHandle,
    url: String,
    path: Option<String>,
    quality: Option<Quality>,
    mode: Option<DownloadMode>,
) -> Result<PlaylistDownloadResult, String> {
    let _guard = crate::functions::youtube::DownloadGuard::new();
    let app_opt = Some(app.clone());

    let base_dir = resolve_out_dir(&app, path);

    tokio::fs::create_dir_all(&base_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    // Relative -o template (yt-dlp creates the playlist subfolder itself),
    // directories set via -P. An absolute path in -o would make yt-dlp ignore --paths.
    let out_tmpl = "%(playlist_title)s/%(playlist_index)02d - %(title)s.%(ext)s".to_string();

    let tmp_dir = base_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;

    let mut args: Vec<String> = vec![
        "--yes-playlist".into(),
        "--no-warnings".into(),
        "-P".into(),
        format!("home:{}", base_dir.to_string_lossy()),
        "-P".into(),
        format!("temp:{}", tmp_dir.to_string_lossy()),
        "-o".into(),
        out_tmpl,
    ];
    args.extend(network_args());

    match mode.unwrap_or(DownloadMode::Video) {
        DownloadMode::Audio => {
            args.extend([
                "-f".into(),
                "bestaudio".into(),
                "-x".into(),
                "--audio-format".into(),
                "mp3".into(),
                "--audio-quality".into(),
                "0".into(),
            ]);
        }
        DownloadMode::Video => {
            let fmt = quality_to_format(quality.unwrap_or(Quality::Best));
            args.extend([
                "-f".into(),
                fmt.into(),
                "--merge-output-format".into(),
                "mp4".into(),
            ]);
        }
    }

    args.push(url.clone());

    let status =
        run_ytdlp_status(args, "Playlist download failed".into(), app_opt.clone()).await?;

    cleanup_temp(&base_dir).await;
    // Remove the temp dir before counting files / locating the playlist folder,
    // otherwise newest_subdir could mistake it for a downloads folder.
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    if !status.success() {
        return Err("Playlist download failed — check logs for details".into());
    }

    let (downloaded, total_size_mb) = count_downloaded(&base_dir).await;

    let playlist_dir = newest_subdir(&base_dir).await.unwrap_or(base_dir);

    Ok(PlaylistDownloadResult {
        dir: playlist_dir.to_string_lossy().into(),
        downloaded,
        total: downloaded,
        total_size_mb,
    })
}

async fn count_downloaded(dir: &std::path::Path) -> (u64, f64) {
    let mut count = 0u64;
    let mut size = 0f64;
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return (0, 0.0);
    };
    while let Ok(Some(e)) = rd.next_entry().await {
        let p = e.path();
        if p.is_dir() {
            let (c, s) = count_downloaded_sync(&p);
            count += c;
            size += s;
        }
    }
    (count, size)
}

fn count_downloaded_sync(dir: &std::path::Path) -> (u64, f64) {
    let mut count = 0u64;
    let mut size = 0f64;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (0, 0.0);
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_file() {
            if let Ok(m) = p.metadata() {
                count += 1;
                size += file_mb(m.len());
            }
        }
    }
    (count, size)
}

async fn newest_subdir(dir: &std::path::Path) -> Option<PathBuf> {
    let mut rd = tokio::fs::read_dir(dir).await.ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Ok(Some(e)) = rd.next_entry().await {
        let p = e.path();
        if p.is_dir() {
            if let Ok(m) = tokio::fs::metadata(&p).await {
                if let Ok(t) = m.modified() {
                    if best.as_ref().map_or(true, |(bt, _)| t > *bt) {
                        best = Some((t, p));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}