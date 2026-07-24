# Voice Input for Omarchy

This repo implements an Omarchy-native `voice-input` wrapper/service for Arch Linux on Hyprland/Wayland.

It provides a distinct custom command and integration namespace while retaining Omarchy's stock Voxtype only as an explicit local-ASR fallback:

- `~/.config/voice-input/config.toml`
- `voice-input.service` as a `systemd --user` unit
- `voice-input record start|stop|toggle` for Hyprland bindings
- `voice-input hud move|position|center|reset` for live HUD placement control
- `voice-input status --follow --format json` for Waybar
- `/usr/bin/voxtype` only when the configured local fallback is needed

## Architecture

- Rust core daemon:
  - captures microphone audio through `pw-record`
  - exposes a Unix-socket control API for `record start|stop|toggle|cancel`
  - writes low-frequency Waybar/HUD status to `$XDG_RUNTIME_DIR/voice-input/state.json`
  - streams 62.5 FPS center-mirrored waveform frames over `$XDG_RUNTIME_DIR/voice-input/waveform.sock`, independently of transcript persistence and Qwen packetization
  - can keep a short background microphone pre-roll ring buffer so speech spoken immediately after trigger is preserved
  - supports two ASR providers:
    - `local-cli`: transcribes through `/usr/bin/voxtype transcribe`, so the installed Omarchy/Arch backend remains swappable and model-capable
    - `alibaba-qwen-realtime`: streams PCM chunks over a WebSocket session to `qwen3-asr-flash-realtime`
  - keeps the full audio buffer locally so remote ASR can:
    - run a second-pass full-audio retranscription through `qwen3-asr-flash`
    - fall back to the local CLI backend when configured
  - optionally refines final text through an OpenAI-compatible `chat/completions` endpoint
  - can capture the focused Kitty Pi/Codex session at recording start and use a redacted excerpt of its latest completed assistant message as terminology-only refinement context
  - injects short text with `wtype`, and automatically switches long text or XWayland clients to `wl-clipboard` + synthetic `Shift+Insert` while preserving clipboard contents
  - temporarily disables active `fcitx5` input mode before synthetic output and restores it afterwards

- Quickshell HUD layer:
  - `assets/quickshell/` provides a Qt Quick/QML HUD using Wayland layer-shell
  - `voice-input-hud.service` keeps the shell resident and invisible while idle for instant display on recording
  - the HUD polls `$XDG_RUNTIME_DIR/voice-input/state.json` for phase/transcript changes, consumes 30-bar waveform NDJSON through Quickshell's `QLocalSocket`, and follows the active Omarchy palette from `~/.config/omarchy/current/theme/colors.toml`; long live transcripts scroll upward inside a fixed-height clipped viewport so the newest text remains visible; the HUD ignores pointer/keyboard input and follows the focused Hyprland monitor
  - live placement comes from the daemon snapshot, so `voice-input hud ...` commands can reposition it without restarting either service
- Python GTK4 compatibility layer:
  - `assets/hud.py` remains available as a rollback frontend when `VOICE_INPUT_EXTERNAL_HUD` is not enabled
  - `assets/settings.py` provides a libadwaita preferences window for language, hotkey, backend, credentials, and LLM settings

## Credentials and publication safety

- Repository configuration under `assets/config.toml` contains no API keys; it is a public sample.
- Runtime secrets belong in the per-user systemd encrypted credential store at `~/.config/credstore.encrypted/` and must never be copied into the repository.
- `.gitignore` excludes local agent metadata, environment files, credentials and private keys, logs, databases, session files, recordings, backups, and build output.
- Agent context is opt-in, read only at runtime, redacted/capped before use, and is never persisted in this repository.
- Locally built artifacts are not committed. Build paths and user-specific home directories are not embedded in the release binary.

## Data flow and privacy

- `local-cli` ASR sends audio only to the configured local backend command. Voice Input itself performs no telemetry or analytics collection.
- Alibaba Qwen realtime and final-pass modes send captured audio to the configured Alibaba endpoint.
- LLM refinement sends the transcript to the configured OpenAI-compatible provider. When agent context is explicitly enabled, a capped and redacted excerpt is sent with it as untrusted terminology reference.
- Agent context and LLM refinement are disabled in the public sample configuration. Runtime transcripts, audio, state, and agent session data are not stored in this repository.

## Independent project

Voice Input is an independent community project. It is not affiliated with or endorsed by Omarchy, Alibaba, OpenAI, OpenRouter, Pi, or Codex.

## Dependency choices

- PipeWire capture uses `pw-record` instead of a native PipeWire Rust binding because this machine has the runtime tools and GTK stack installed, but not the PipeWire development headers.
- Optional pre-roll uses a persistent background `pw-record` stream. That improves trigger responsiveness, but it means the microphone stays open while `voice-input.service` is running.
- Speech recognition stays modular:
  - local mode delegates to `/usr/bin/voxtype`, which already supports Whisper and ONNX-family engines such as SenseVoice and Paraformer
  - remote mode uses Alibaba's official realtime WebSocket protocol for `qwen3-asr-flash-realtime`
  - optional final-pass retranscription uses Alibaba's OpenAI-compatible `qwen3-asr-flash` file-audio API on the saved WAV buffer
- The default ASR choice here is `sensevoice` with Simplified Chinese, because this target environment prioritizes Chinese and mixed Chinese-English dictation.

## Hotkey strategy on Hyprland

Press/release binding:

```conf
bind = SUPER CTRL, X, exec, voice-input record start
bindr = SUPER CTRL, X, exec, voice-input record stop
```

This uses Hyprland's compositor-native press/release support and avoids X11 global hotkey hacks.
In practice, it is only reliable if your release pattern lets Hyprland observe the trigger key release while the modifier state still matches.

Toggle fallback:

```conf
bindd = SUPER CTRL, X, Voice input, exec, voice-input record toggle
```

Use the fallback when `bindr` proves unreliable on a specific setup. Multi-modifier bindings can miss the release event depending on key release order, so toggle mode is the robust Hyprland fallback for `Super+Ctrl+X`.

Optional live HUD nudging:

```conf
bind = SUPER CTRL ALT, left, exec, voice-input hud move left
bind = SUPER CTRL ALT, right, exec, voice-input hud move right
bind = SUPER CTRL ALT, up, exec, voice-input hud move up
bind = SUPER CTRL ALT, down, exec, voice-input hud move down
bind = SUPER CTRL ALT, c, exec, voice-input hud center
```

These bindings are safe to keep globally. They only matter while the overlay is visible.

## IME and output strategy

- Primary output path: `wtype`
- Clipboard fallback and long-text path: `wl-copy` + synthetic `Shift+Insert`, matching Omarchy's Universal Paste behavior and Kitty's `paste_from_clipboard` mapping
- XWayland path: for reliable XWayland paste, `voice-input` needs `xclip` and `xdotool`. It uses `xclip` to populate the X clipboard and `xdotool key --clearmodifiers ...` for the paste chord. The default XWayland paste chord is `Shift+Insert`, which avoids `Ctrl` interference from modifier-heavy toggle hotkeys like `Super+Ctrl+X`
- Clipboard fallback preserves clipboard contents and restores them after the paste
- `fcitx5-remote` is used to temporarily force ASCII-capable output when needed and then restore the original state

## Platform limitations

- In `local-cli` mode, partial transcript updates are near-real-time, not true token streaming. The daemon periodically retranscribes the current buffer while recording.
- In `alibaba-qwen-realtime` mode, partial updates are true server-streamed preview text assembled from the provider's `text + stash` events.
- If `audio.pre_roll_enabled = true`, the daemon keeps a warm capture stream running in the background. This is the intended tradeoff for preserving speech spoken immediately after trigger.
- The primary HUD requires Quickshell 0.3+ (`qs`) with Qt 6 and Wayland layer-shell support. The settings window and rollback HUD still depend on Python GTK bindings (`PyGObject`, `Gtk4`, `libadwaita`, `Gtk4LayerShell`).
- Local fallback assumes `/usr/bin/voxtype` exists as the backend engine binary.

## Realtime ASR Notes

- Recommended remote model: `qwen3-asr-flash-realtime-2026-02-10`
- Recommended turn mode: `server-vad`
- Recommended final-pass model: `qwen3-asr-flash-2026-02-10`
- Final flow with remote ASR:
  1. `pw-record` captures 16 kHz mono PCM
  2. the daemon forwards chunks to the Alibaba realtime session
  3. partial transcript events update the HUD live
  4. on stop, the daemon briefly drains pending speech events; if Qwen detected no speech, it cancels the empty session and returns directly to idle without ASR, LLM refinement, or text output; otherwise it sends `session.finish`
  5. if `final_pass_enabled = true`, the daemon retranscribes the saved full WAV buffer through `qwen3-asr-flash`
  6. if the final pass fails, the daemon falls back to the realtime final transcript, then to the local CLI backend when `fallback_to_local = true`
  7. optional LLM refinement runs after the final ASR text is chosen, not in parallel with it
  8. when `llm.agent_context_enabled = true`, the daemon uses the Pi or Codex session that was focused at recording start as terminology-only reference context

Example remote config:

```toml
[asr]
provider = "alibaba-qwen-realtime"
language = "simplified-chinese"
fallback_to_local = true

[asr.alibaba]
endpoint = "wss://dashscope.aliyuncs.com/api-ws/v1/realtime"
model = "qwen3-asr-flash-realtime-2026-02-10"
turn_mode = "server-vad"
final_pass_enabled = true
final_pass_base_url = ""
final_pass_model = "qwen3-asr-flash-2026-02-10"
final_pass_timeout_ms = 20000
final_pass_enable_itn = false
```

`final_pass_base_url = ""` means “derive the compatible-mode HTTP base URL from the configured realtime region endpoint”.

Agent-aware refinement is opt-in because the reference excerpt is sent to the configured LLM provider:

```toml
[llm]
provider_sort = "latency"
agent_context_enabled = true
agent_context_max_chars = 6000
```

When the API base URL belongs to OpenRouter, `provider_sort = "latency"` adds `provider.sort = "latency"` to each refinement request. The option is intentionally ignored for other OpenAI-compatible providers so they never receive an OpenRouter-specific field.

Pi integration installs `~/.pi/agent/extensions/voice-input-session-registry.ts`. Existing Pi processes must run `/reload` once after installation; future Pi sessions load it automatically. Codex needs no extension because the daemon identifies the focused Codex process and its main CLI rollout directly. Context discovery or parsing failures silently fall back to transcript-only refinement. The reference is length-limited, obvious secret-bearing lines and token patterns are redacted, and its contents are explicitly treated as untrusted terminology reference rather than instructions.

API credentials are not stored in `config.toml`. The systemd user service optionally loads encrypted credentials named `alibaba-api-key` and `openrouter-api-key` from the standard per-user store at `~/.config/credstore.encrypted/`. Missing credentials are non-fatal, so fresh installs and local-only ASR work without secret setup. The settings UI can create or replace encrypted credentials without showing existing values. For development outside systemd, `VOICE_INPUT_ALIBABA_API_KEY` and `VOICE_INPUT_OPENROUTER_API_KEY` are supported as fallbacks.

## Build and install

```bash
make build
make install
make enable-service
```

After install:

1. Install Quickshell (`omarchy pkg add quickshell` on Omarchy)
2. Run `make enable-service` to enable both `voice-input.service` and `voice-input-hud.service`
3. Source or paste the Hyprland snippet from `~/.local/share/voice-input/omarchy-hyprland-snippet.conf`
4. Keep or add the Waybar module shown in `~/.local/share/voice-input/omarchy-waybar-snippet.jsonc`
5. Restart Hyprland bindings and Waybar

## License

Licensed under the [MIT License](LICENSE). Copyright (c) 2026 Saco Song.
