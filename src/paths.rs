use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow};

pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| anyhow!("XDG config directory not found"))?;
    Ok(base.join("voice-input"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn runtime_dir() -> Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(dirs::runtime_dir)
        .ok_or_else(|| anyhow!("XDG runtime directory not found"))?;
    Ok(base.join("voice-input"))
}

pub fn control_socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("control.sock"))
}

pub fn current_executable() -> Result<PathBuf> {
    env::current_exe().context("failed to determine current executable path")
}

pub fn asset_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("VOICE_INPUT_ASSET_DIR") {
        return Ok(PathBuf::from(path));
    }

    let exe = current_executable()?;
    if let Some(bin_dir) = exe.parent() {
        let share_dir = bin_dir
            .parent()
            .map(|parent| parent.join("share").join("voice-input"));
        if let Some(share_dir) = share_dir.filter(|path| path.exists()) {
            return Ok(share_dir);
        }
    }

    let source_assets = env::current_dir()
        .context("failed to determine current directory")?
        .join("assets");
    if source_assets.exists() {
        return Ok(source_assets);
    }

    Err(anyhow!(
        "Voice Input assets were not found beside the installation or in ./assets"
    ))
}

pub fn quickshell_settings_path() -> Result<PathBuf> {
    Ok(asset_dir()?.join("quickshell-settings"))
}

pub fn waybar_snippet_path() -> Result<PathBuf> {
    Ok(asset_dir()?.join("omarchy-waybar-snippet.jsonc"))
}
