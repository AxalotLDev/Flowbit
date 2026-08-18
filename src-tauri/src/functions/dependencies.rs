use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use yt_dlp::Downloader;

static LIBS_PATH: OnceLock<String> = OnceLock::new();

/// Whether the binaries (yt-dlp/ffmpeg/…) are ready. Until then the frontend
/// shows a loading screen instead of a blank "stuck" window.
pub static DEPS_READY: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub fn deps_ready() -> bool {
    DEPS_READY.load(Ordering::SeqCst)
}

pub fn init_libs_dir_path(path: &PathBuf) {
    LIBS_PATH.set(path.to_string_lossy().to_string()).ok();
}

#[inline]
pub fn libs_dir() -> &'static str {
    LIBS_PATH.get().expect("LIBS_PATH not initialized yet")
}

/// Joins a path inside libs using the native OS separator. MUST NOT use
/// `format!("{}/{}")`: on Windows that produces a mixed-separator path
/// (`C:\...\libs/yt-dlp.exe`), and CreateProcess can't find the file (os error 2).
#[inline]
fn lib_path(file_name: &str) -> String {
    Path::new(libs_dir())
        .join(file_name)
        .to_string_lossy()
        .into_owned()
}

#[inline]
pub fn yt_dlp() -> String {
    lib_path(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" })
}

#[inline]
pub fn ffmpeg() -> String {
    lib_path(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" })
}

#[inline]
pub fn quickjs() -> String {
    let file_name = if cfg!(windows) {
        if cfg!(target_arch = "x86_64") {
            "qjs-windows-x86_64.exe"
        } else {
            "qjs-windows-x86.exe"
        }
    } else if cfg!(target_os = "macos") {
        "qjs-darwin"
    } else if cfg!(target_arch = "aarch64") {
        "qjs-linux-aarch64"
    } else if cfg!(target_arch = "x86") {
        "qjs-linux-x86"
    } else {
        "qjs-linux-x86_64"
    };

    lib_path(file_name)
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub async fn download_quickjs(libs_dir: &Path) -> Result<(), String> {
    let asset_name = if cfg!(windows) {
        if cfg!(target_arch = "x86_64") {
            "qjs-windows-x86_64.exe"
        } else {
            "qjs-windows-x86.exe"
        }
    } else if cfg!(target_os = "macos") {
        "qjs-darwin"
    } else if cfg!(target_arch = "aarch64") {
        "qjs-linux-aarch64"
    } else if cfg!(target_arch = "x86") {
        "qjs-linux-x86"
    } else {
        "qjs-linux-x86_64"
    };

    let file_path = libs_dir.join(asset_name);

    if file_path.exists() {
        return Ok(());
    }

    let url = "https://api.github.com/repos/quickjs-ng/quickjs/releases/latest";

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 5))
        .build()
        .map_err(|e| e.to_string())?;

    let release: GithubRelease = client
        .get(url)
        .header("User-Agent", "tauri-app")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| format!("QuickJS asset not found: {}", asset_name))?;

    let bytes = reqwest::get(&asset.browser_download_url)
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    fs::write(&file_path, &bytes).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&file_path)
            .map_err(|e| e.to_string())?
            .permissions();

        perms.set_mode(0o755);
        fs::set_permissions(&file_path, perms).map_err(|e| e.to_string())?;
    }

    Ok(())
}

const FFPROBE_BUILD_VERSION: &str = "6.1";

/// Platform slug for ffbinaries matching the current OS/arch. ffbinaries
/// publishes each binary as its own zip (the archive contains exactly one
/// ffprobe[.exe] file), unlike boul2gom/ffmpeg-builds, whose archive only
/// has ffmpeg.
fn ffprobe_platform() -> &'static str {
    if cfg!(windows) {
        "win-64"
    } else if cfg!(target_os = "macos") {
        "macos-64"
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm-64"
    } else if cfg!(target_arch = "x86") {
        "linux-32"
    } else {
        "linux-64"
    }
}

/// Downloads ffprobe alongside ffmpeg. The yt-dlp crate only ships ffmpeg,
/// but yt-dlp looks for ffprobe in the same directory (otherwise "WARNING:
/// ffprobe not found"). Fetched as a separate zip from ffbinaries-prebuilt.
pub async fn download_ffprobe(libs_dir: &Path) -> Result<(), String> {
    let file_name = if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" };
    let dest = libs_dir.join(file_name);
    if dest.exists() {
        return Ok(());
    }

    let asset = format!(
        "ffprobe-{FFPROBE_BUILD_VERSION}-{}.zip",
        ffprobe_platform()
    );
    let url = format!(
        "https://github.com/ffbinaries/ffbinaries-prebuilt/releases/download/v{FFPROBE_BUILD_VERSION}/{asset}"
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 10))
        .build()
        .map_err(|e| e.to_string())?;

    let bytes = client
        .get(&url)
        .header("User-Agent", "tauri-app")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    // The archive is flat — find the entry ending in ffprobe(.exe).
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut found = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let base = entry.name().rsplit(['/', '\\']).next().unwrap_or("");
        if base == file_name {
            found = Some(i);
            break;
        }
    }
    let idx = found.ok_or_else(|| format!("{file_name} not found in {asset}"))?;

    let mut entry = archive.by_index(idx).map_err(|e| e.to_string())?;
    let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    drop(out);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms).map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// App data root. On Windows a short `%APPDATA%\flowbit` (no reverse-domain
/// identifier); on other platforms Tauri's standard app_data_dir
/// (`.../com.axalotl.flowbit`).
pub fn app_data_root(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(windows)]
    if let Some(base) = dirs::data_dir() {
        return Ok(base.join("flowbit"));
    }
    app.path().app_data_dir().map_err(|e| e.to_string())
}

pub async fn install_dependencies(app: &AppHandle) -> Result<(), String> {
    let data_dir = app_data_root(app)?;

    let libs_dir = data_dir.join("libs");
    let output_dir = data_dir.join("output");

    fs::create_dir_all(&libs_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;

    init_libs_dir_path(&libs_dir);

    // Fast path: if yt-dlp and ffmpeg are already present, don't re-download
    // them (otherwise the loading screen would flash on every launch). The
    // background self-update after startup keeps yt-dlp fresh.
    let yt = libs_dir.join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" });
    let ff = libs_dir.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
    if !(yt.exists() && ff.exists()) {
        Downloader::with_new_binaries(libs_dir.clone(), output_dir)
            .await
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| e.to_string())?;
    }

    download_quickjs(&libs_dir).await?;
    // ffprobe is best-effort: downloading still works without it, just with a warning.
    if let Err(e) = download_ffprobe(&libs_dir).await {
        eprintln!("ffprobe download failed: {e}");
    }
    DEPS_READY.store(true, Ordering::SeqCst);
    Ok(())
}
