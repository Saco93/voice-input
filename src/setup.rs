use std::{
    fs,
    io::{self, Write},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::{
    config::{AlibabaTurnMode, AsrProvider, Config, HotkeyMode},
    paths,
};

pub fn run_model_wizard(config: &mut Config) -> Result<()> {
    println!("Voice Input model setup");
    println!();
    println!("Select language:");
    let languages = [
        crate::config::Language::English,
        crate::config::Language::SimplifiedChinese,
        crate::config::Language::TraditionalChinese,
        crate::config::Language::Japanese,
        crate::config::Language::Korean,
    ];
    for (index, language) in languages.iter().enumerate() {
        println!("  {}. {}", index + 1, language.label());
    }
    print!("Language [{}]: ", config.asr.language.label());
    io::stdout().flush().ok();
    let selected_language = read_line()?;
    if !selected_language.trim().is_empty() {
        let position: usize = selected_language
            .trim()
            .parse()
            .context("expected a number")?;
        config.asr.language = *languages
            .get(position.saturating_sub(1))
            .ok_or_else(|| anyhow::anyhow!("language selection out of range"))?;
    }

    println!();
    println!("ASR provider:");
    println!("  1. local-cli              (current /usr/bin/voxtype backend)");
    println!("  2. alibaba-qwen-realtime  (true streaming partials over WebSocket)");
    print!(
        "Provider [{}]: ",
        match config.asr.provider {
            AsrProvider::LocalCli => "local-cli",
            AsrProvider::AlibabaQwenRealtime => "alibaba-qwen-realtime",
        }
    );
    io::stdout().flush().ok();
    let provider = read_line()?;
    match provider.trim() {
        "" => {}
        "1" | "local-cli" => config.asr.provider = AsrProvider::LocalCli,
        "2" | "alibaba-qwen-realtime" => config.asr.provider = AsrProvider::AlibabaQwenRealtime,
        value => bail!("unknown ASR provider `{value}`"),
    }

    match config.asr.provider {
        AsrProvider::LocalCli => {
            println!();
            println!("Suggested local engines:");
            println!("  1. sensevoice  (best default for zh/en/ja/ko)");
            println!("  2. paraformer  (good zh+en bilingual fallback)");
            println!("  3. whisper     (generic multilingual fallback)");
            print!("Engine [{}]: ", config.asr.engine);
            io::stdout().flush().ok();
            let engine = read_line()?;
            match engine.trim() {
                "" => {}
                "1" => config.asr.engine = "sensevoice".into(),
                "2" => config.asr.engine = "paraformer".into(),
                "3" => config.asr.engine = "whisper".into(),
                value => config.asr.engine = value.to_string(),
            }

            print!(
                "Model [{}] (leave empty for backend default): ",
                if config.asr.model.is_empty() {
                    "default"
                } else {
                    &config.asr.model
                }
            );
            io::stdout().flush().ok();
            let model = read_line()?;
            if !model.trim().is_empty() {
                config.asr.model = model.trim().to_string();
            }
        }
        AsrProvider::AlibabaQwenRealtime => {
            print!("Alibaba model [{}]: ", config.asr.alibaba.model);
            io::stdout().flush().ok();
            let model = read_line()?;
            if !model.trim().is_empty() {
                config.asr.alibaba.model = model.trim().to_string();
            }

            print!("Endpoint [{}]: ", config.asr.alibaba.endpoint);
            io::stdout().flush().ok();
            let endpoint = read_line()?;
            if !endpoint.trim().is_empty() {
                config.asr.alibaba.endpoint = endpoint.trim().to_string();
            }

            println!("Turn mode:");
            println!("  1. server-vad  (recommended default)");
            println!("  2. manual      (commit on stop)");
            print!(
                "Turn mode [{}]: ",
                match config.asr.alibaba.turn_mode {
                    AlibabaTurnMode::ServerVad => "server-vad",
                    AlibabaTurnMode::Manual => "manual",
                }
            );
            io::stdout().flush().ok();
            let turn_mode = read_line()?;
            match turn_mode.trim() {
                "" => {}
                "1" | "server-vad" => {
                    config.asr.alibaba.turn_mode = AlibabaTurnMode::ServerVad;
                }
                "2" | "manual" => {
                    config.asr.alibaba.turn_mode = AlibabaTurnMode::Manual;
                }
                value => bail!("unknown turn mode `{value}`"),
            }

            print!(
                "Fallback to local CLI on remote failure [{}] (y/n): ",
                if config.asr.fallback_to_local {
                    "y"
                } else {
                    "n"
                }
            );
            io::stdout().flush().ok();
            let fallback = read_line()?;
            match fallback.trim().to_ascii_lowercase().as_str() {
                "" => {}
                "y" | "yes" => config.asr.fallback_to_local = true,
                "n" | "no" => config.asr.fallback_to_local = false,
                value => bail!("unknown fallback choice `{value}`"),
            }
        }
    }

    config.save()?;
    println!();
    println!("Saved {}", paths::config_path()?.display());
    Ok(())
}

pub fn print_waybar_snippet() -> Result<()> {
    let snippet = fs::read_to_string(paths::waybar_snippet_path()?)
        .context("failed to load Waybar snippet")?;
    println!("{snippet}");
    Ok(())
}

pub fn print_hyprland_snippet() -> Result<()> {
    let config = Config::load()?;
    println!("# Hyprland binding snippet for Voice Input on Omarchy.");
    println!("# Add this to ~/.config/hypr/bindings.conf or source it from hyprland.conf.");
    println!("#");
    println!("# Keep the HUD from taking focus if Hyprland maps it as a regular client:");
    println!("windowrule = no_focus on, match:title ^Voice Input HUD$");
    println!("windowrule = no_anim on, match:title ^Voice Input HUD$");
    println!("# Float Settings at its requested utility-window size:");
    println!(
        "windowrule = float on, match:class ^org\\.quickshell$, match:title ^Voice Input Settings$"
    );
    println!(
        "windowrule = center on, match:class ^org\\.quickshell$, match:title ^Voice Input Settings$"
    );
    println!("#");
    println!("# Add adjacent F-key controls without changing Omarchy's stock Super+Ctrl+X:");
    println!("unbind = , F8");
    println!("unbind = , F9");
    println!("unbind = , F10");
    println!("binddp = , F8, Cancel voice input, exec, voice-input record cancel");
    println!(
        "binddp = , F10, Restart voice input, exec, bash -lc 'voice-input record cancel && voice-input record start'"
    );
    println!("#");
    match config.hotkey.mode {
        HotkeyMode::Hold => {
            println!("# Press to start recording, release the trigger key first to finalize:");
            println!(
                "bind = {}, exec, voice-input record start",
                config.hotkey.accelerator
            );
            println!(
                "bindr = {}, exec, voice-input record stop",
                config.hotkey.accelerator
            );
            println!("#");
            println!("# Optional live HUD nudging while the overlay is visible:");
            println!("bind = SUPER CTRL ALT, left, exec, voice-input hud move left");
            println!("bind = SUPER CTRL ALT, right, exec, voice-input hud move right");
            println!("bind = SUPER CTRL ALT, up, exec, voice-input hud move up");
            println!("bind = SUPER CTRL ALT, down, exec, voice-input hud move down");
            println!("bind = SUPER CTRL ALT, c, exec, voice-input hud center");
            println!("#");
            println!("# Toggle fallback if press/release is not reliable on your setup:");
            println!(
                "# binddp = {}, Voice input, exec, voice-input record toggle",
                config.hotkey.accelerator
            );
        }
        HotkeyMode::Toggle => {
            println!("# Toggle mode is the robust fallback on Hyprland when bindr is unreliable:");
            println!(
                "binddp = {}, Voice input, exec, voice-input record toggle",
                config.hotkey.accelerator
            );
            println!("#");
            println!("# Optional press/release version if your setup handles bindr reliably:");
            println!(
                "# bind = {}, exec, voice-input record start",
                config.hotkey.accelerator
            );
            println!(
                "# bindr = {}, exec, voice-input record stop",
                config.hotkey.accelerator
            );
            println!("#");
            println!("# Optional live HUD nudging while the overlay is visible:");
            println!("bind = SUPER CTRL ALT, left, exec, voice-input hud move left");
            println!("bind = SUPER CTRL ALT, right, exec, voice-input hud move right");
            println!("bind = SUPER CTRL ALT, up, exec, voice-input hud move up");
            println!("bind = SUPER CTRL ALT, down, exec, voice-input hud move down");
            println!("bind = SUPER CTRL ALT, c, exec, voice-input hud center");
        }
    }
    Ok(())
}

pub fn install_systemd_unit() -> Result<()> {
    let unit_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("XDG config directory not found"))?
        .join("systemd/user");
    fs::create_dir_all(&unit_dir)?;

    let asset_dir = paths::asset_dir()?;
    let binary = paths::current_executable()?.display().to_string();
    let daemon_template = fs::read_to_string(asset_dir.join("voice-input.service"))
        .context("failed to load Voice Input daemon service template")?;
    let daemon_rendered = daemon_template
        .replace("@VOICE_INPUT_BIN@", &binary)
        .replace("@VOICE_INPUT_ASSET_DIR@", &asset_dir.display().to_string());
    let daemon_target = unit_dir.join("voice-input.service");
    fs::write(&daemon_target, daemon_rendered)
        .with_context(|| format!("failed to write {}", daemon_target.display()))?;

    let hud_template = fs::read_to_string(asset_dir.join("voice-input-hud.service"))
        .context("failed to load Voice Input HUD service template")?;
    let hud_rendered = hud_template.replace(
        "@VOICE_INPUT_QUICKSHELL_DIR@",
        &asset_dir.join("quickshell").display().to_string(),
    );
    let hud_target = unit_dir.join("voice-input-hud.service");
    fs::write(&hud_target, hud_rendered)
        .with_context(|| format!("failed to write {}", hud_target.display()))?;

    println!("Installed {}", daemon_target.display());
    println!("Installed {}", hud_target.display());
    Ok(())
}

pub fn proxy_backend_setup(args: &[String]) -> Result<()> {
    let config = Config::load()?;
    let status = Command::new(&config.asr.backend_command)
        .arg("setup")
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "failed to run backend setup via {}",
                config.asr.backend_command
            )
        })?;

    if status.success() {
        Ok(())
    } else {
        bail!("backend setup command exited with status {status}")
    }
}

fn read_line() -> Result<String> {
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value)
}
