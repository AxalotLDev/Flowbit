use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use yt_dlp::Downloader;

static LIBS_PATH: OnceLock<String> = OnceLock::new();

pub fn init_libs_dir_path(path: &PathBuf) {
    LIBS_PATH.set(path.to_string_lossy().to_string()).ok();
}

#[inline]
pub fn libs_dir() -> &'static str {
    LIBS_PATH.get().expect("LIBS_PATH not initialized yet")
}

/// Собирает путь к файлу в libs через нативный разделитель ОС. НЕЛЬЗЯ использовать
/// `format!("{}/{}")`: на Windows получается путь со смешанными разделителями
/// (`C:\...\libs/yt-dlp.exe`), и CreateProcess не находит файл (os error 2).
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

/// Базовый каталог данных приложения. На Windows — короткий `%APPDATA%\flowbit`
/// (без reverse-domain идентификатора), на остальных платформах — стандартный
/// app_data_dir от Tauri (`.../com.axalotl.flowbit`).
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

    Downloader::with_new_binaries(libs_dir.clone(), output_dir)
        .await
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;

    download_quickjs(&libs_dir).await?;
    Ok(())
}
