use std::fs;
use std::path::Path;

use serde::Deserialize;

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
    // 1. определяем имя бинарника под платформу
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

    // уже скачан
    if file_path.exists() {
        return Ok(());
    }

    // 2. получаем latest release GitHub
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

    // 3. ищем нужный asset
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| format!("QuickJS asset not found: {}", asset_name))?;

    // 4. скачиваем файл
    let bytes = reqwest::get(&asset.browser_download_url)
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    fs::write(&file_path, &bytes).map_err(|e| e.to_string())?;

    // 5. делаем исполняемым (Linux/macOS)
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
