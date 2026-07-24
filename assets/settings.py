#!/usr/bin/env python3

import argparse
import json
import os
import subprocess
import tempfile
import tomllib
import urllib.request
import urllib.error
from pathlib import Path

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")

from gi.repository import Adw, Gio, Gtk  # noqa: E402


LANGUAGE_VALUES = [
    ("English", "english"),
    ("Simplified Chinese", "simplified-chinese"),
    ("Traditional Chinese", "traditional-chinese"),
    ("Japanese", "japanese"),
    ("Korean", "korean"),
]

HOTKEY_MODES = [
    ("Hold (bind + bindr recommended)", "hold"),
    ("Toggle fallback", "toggle"),
]

PROVIDER_VALUES = [
    ("Local CLI backend", "local-cli"),
    ("Alibaba Qwen realtime", "alibaba-qwen-realtime"),
]

TURN_MODE_VALUES = [
    ("Server VAD", "server-vad"),
    ("Manual commit", "manual"),
]

HUD_POSITION_VALUES = [
    ("Bottom Center", "bottom-center"),
    ("Bottom Left", "bottom-left"),
    ("Bottom Right", "bottom-right"),
]


def escape_toml(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')


class SettingsWindow(Adw.PreferencesWindow):
    def __init__(self, config_path: Path, binary_path: Path) -> None:
        super().__init__()
        self.config_path = config_path
        self.binary_path = binary_path
        self.set_title("Voice Input Settings")
        self.set_default_size(760, 760)

        self.loaded = self.load_config()

        page = Adw.PreferencesPage()

        general = Adw.PreferencesGroup(title="General")
        self.language_combo = self.make_combo(
            general, "Language", LANGUAGE_VALUES, self.loaded["asr"]["language"]
        )
        self.hotkey_entry = self.make_entry(
            general, "Hotkey", self.loaded["hotkey"]["accelerator"]
        )
        self.mode_combo = self.make_combo(
            general, "Hotkey Mode", HOTKEY_MODES, self.loaded["hotkey"]["mode"]
        )

        audio = Adw.PreferencesGroup(title="Audio")
        self.pre_roll_switch = Adw.SwitchRow(
            title="Keep a short microphone pre-roll buffer"
        )
        self.pre_roll_switch.set_subtitle(
            "Keeps the mic capture path warm so speech right after trigger is preserved. This keeps the microphone open while the daemon is running."
        )
        self.pre_roll_switch.set_active(self.loaded["audio"]["pre_roll_enabled"])
        audio.add(self.pre_roll_switch)
        self.pre_roll_ms = self.make_entry(
            audio,
            "Pre-Roll Window (ms)",
            str(self.loaded["audio"]["pre_roll_ms"]),
        )

        asr = Adw.PreferencesGroup(title="Speech Recognition")
        self.provider_combo = self.make_combo(
            asr, "Provider", PROVIDER_VALUES, self.loaded["asr"]["provider"]
        )
        self.backend_command = self.make_entry(
            asr, "Backend Command", self.loaded["asr"]["backend_command"]
        )
        self.engine_entry = self.make_entry(asr, "Local Engine", self.loaded["asr"]["engine"])
        self.model_entry = self.make_entry(asr, "Local Model", self.loaded["asr"]["model"])
        self.connect_timeout = self.make_entry(
            asr, "Connect Timeout (ms)", str(self.loaded["asr"]["connect_timeout_ms"])
        )
        self.finalize_timeout = self.make_entry(
            asr, "Finalize Timeout (ms)", str(self.loaded["asr"]["finalize_timeout_ms"])
        )
        self.local_fallback = Adw.SwitchRow(title="Fallback to local CLI if remote ASR fails")
        self.local_fallback.set_active(self.loaded["asr"]["fallback_to_local"])
        asr.add(self.local_fallback)

        alibaba = Adw.PreferencesGroup(title="Alibaba Realtime")
        self.alibaba_endpoint = self.make_entry(
            alibaba, "Endpoint", self.loaded["asr"]["alibaba"]["endpoint"]
        )
        self.alibaba_api_key = self.make_password_entry(
            alibaba,
            self.credential_title("Alibaba API Key", "alibaba-api-key"),
        )
        self.alibaba_model = self.make_entry(
            alibaba, "Model", self.loaded["asr"]["alibaba"]["model"]
        )
        self.alibaba_turn_mode = self.make_combo(
            alibaba,
            "Turn Mode",
            TURN_MODE_VALUES,
            self.loaded["asr"]["alibaba"]["turn_mode"],
        )
        self.alibaba_vad_threshold = self.make_entry(
            alibaba,
            "VAD Threshold",
            str(self.loaded["asr"]["alibaba"]["vad_threshold"]),
        )
        self.alibaba_silence_ms = self.make_entry(
            alibaba,
            "Silence Duration (ms)",
            str(self.loaded["asr"]["alibaba"]["silence_duration_ms"]),
        )

        alibaba_final = Adw.PreferencesGroup(title="Alibaba Final Pass")
        self.alibaba_final_pass = Adw.SwitchRow(
            title="Use qwen3-asr-flash for final full-audio retranscription"
        )
        self.alibaba_final_pass.set_active(
            self.loaded["asr"]["alibaba"]["final_pass_enabled"]
        )
        alibaba_final.add(self.alibaba_final_pass)
        self.alibaba_final_base_url = self.make_entry(
            alibaba_final,
            "Final Pass Base URL",
            self.loaded["asr"]["alibaba"]["final_pass_base_url"],
        )
        self.alibaba_final_model = self.make_entry(
            alibaba_final,
            "Final Pass Model",
            self.loaded["asr"]["alibaba"]["final_pass_model"],
        )
        self.alibaba_final_timeout = self.make_entry(
            alibaba_final,
            "Final Pass Timeout (ms)",
            str(self.loaded["asr"]["alibaba"]["final_pass_timeout_ms"]),
        )
        self.alibaba_final_itn = Adw.SwitchRow(
            title="Enable ITN during final pass"
        )
        self.alibaba_final_itn.set_active(
            self.loaded["asr"]["alibaba"]["final_pass_enable_itn"]
        )
        alibaba_final.add(self.alibaba_final_itn)

        llm = Adw.PreferencesGroup(title="LLM Refinement")
        self.llm_switch = Adw.SwitchRow(title="Enable conservative refinement")
        self.llm_switch.set_active(self.loaded["llm"]["enabled"])
        llm.add(self.llm_switch)
        self.api_base_entry = self.make_entry(
            llm, "API Base URL", self.loaded["llm"]["api_base_url"]
        )
        self.api_key_entry = self.make_password_entry(
            llm,
            self.credential_title("OpenRouter API Key", "openrouter-api-key"),
        )
        self.model_llm_entry = self.make_entry(llm, "Model", self.loaded["llm"]["model"])
        self.provider_sort_entry = self.make_entry(
            llm,
            "OpenRouter Provider Sort",
            self.loaded["llm"]["provider_sort"],
        )
        self.agent_context_switch = Adw.SwitchRow(
            title="Use focused Pi/Codex context"
        )
        self.agent_context_switch.set_subtitle(
            "Sends a redacted excerpt of the latest completed assistant message to the refinement provider."
        )
        self.agent_context_switch.set_active(
            self.loaded["llm"]["agent_context_enabled"]
        )
        llm.add(self.agent_context_switch)
        self.agent_context_max_chars = self.make_entry(
            llm,
            "Agent Context Maximum Characters",
            str(self.loaded["llm"]["agent_context_max_chars"]),
        )

        output = Adw.PreferencesGroup(title="Output")
        self.output_mode = self.make_entry(output, "Mode", self.loaded["output"]["mode"])
        self.paste_keys = self.make_entry(output, "Paste Keys", self.loaded["output"]["paste_keys"])
        self.xwayland_paste_keys = self.make_entry(
            output,
            "XWayland Paste Keys",
            self.loaded["output"]["xwayland_paste_keys"],
        )
        self.xwayland_paste_switch = Adw.SwitchRow(
            title="Prefer clipboard paste for XWayland apps"
        )
        self.xwayland_paste_switch.set_subtitle(
            "Avoids garbled direct typing in XWayland clients by using clipboard paste as the primary path there."
        )
        self.xwayland_paste_switch.set_active(
            self.loaded["output"]["prefer_paste_for_xwayland"]
        )
        output.add(self.xwayland_paste_switch)
        self.pre_delay = self.make_entry(
            output, "Pre-Type Delay (ms)", str(self.loaded["output"]["pre_type_delay_ms"])
        )
        self.type_delay = self.make_entry(
            output, "Type Delay (ms)", str(self.loaded["output"]["type_delay_ms"])
        )

        hud = Adw.PreferencesGroup(title="HUD Overlay")
        self.hud_position = self.make_combo(
            hud, "Position", HUD_POSITION_VALUES, self.loaded["hud"]["position"]
        )
        self.hud_margin_bottom = self.make_entry(
            hud, "Bottom Margin", str(self.loaded["hud"]["margin_bottom"])
        )
        self.hud_offset_x = self.make_entry(
            hud, "Horizontal Offset", str(self.loaded["hud"]["offset_x"])
        )
        self.hud_offset_y = self.make_entry(
            hud, "Vertical Offset", str(self.loaded["hud"]["offset_y"])
        )
        self.hud_height = self.make_entry(
            hud, "Base Height", str(self.loaded["hud"]["height"])
        )
        self.hud_nudge_step = self.make_entry(
            hud, "Nudge Step", str(self.loaded["hud"]["nudge_step"])
        )

        integration = Adw.PreferencesGroup(title="Omarchy Integration")
        hint = Adw.ActionRow(
            title="Hyprland",
            subtitle="Use hold mode only if Hyprland reliably delivers `bindr` for your release pattern. Toggle mode is the robust fallback for multi-modifier shortcuts like Super+Ctrl+X. HUD nudging commands are safe to bind globally, but they only matter while the overlay is visible.",
        )
        integration.add(hint)

        buttons = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=12)
        buttons.set_margin_top(12)
        buttons.set_margin_bottom(18)
        buttons.set_margin_start(18)
        buttons.set_margin_end(18)
        buttons.set_halign(Gtk.Align.END)

        self.status = Gtk.Label(xalign=0.0)
        self.status.set_halign(Gtk.Align.START)
        self.status.set_hexpand(True)

        test_button = Gtk.Button(label="Test")
        test_button.connect("clicked", self.on_test_clicked)
        save_button = Gtk.Button(label="Save")
        save_button.add_css_class("suggested-action")
        save_button.connect("clicked", self.on_save_clicked)

        buttons.append(self.status)
        buttons.append(test_button)
        buttons.append(save_button)

        page.add(general)
        page.add(audio)
        page.add(asr)
        page.add(alibaba)
        page.add(alibaba_final)
        page.add(llm)
        page.add(output)
        page.add(hud)
        page.add(integration)

        content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        content.append(page)
        content.append(buttons)
        self.set_content(content)

    def load_config(self) -> dict:
        defaults = {
            "hotkey": {"accelerator": "SUPER CTRL, X", "mode": "hold"},
            "audio": {
                "device": "default",
                "sample_rate": 16000,
                "max_duration_secs": 90,
                "partial_interval_ms": 1500,
                "pre_roll_enabled": False,
                "pre_roll_ms": 500,
            },
            "asr": {
                "provider": "local-cli",
                "backend_command": "/usr/bin/voxtype",
                "engine": "sensevoice",
                "model": "",
                "language": "simplified-chinese",
                "connect_timeout_ms": 5000,
                "finalize_timeout_ms": 8000,
                "fallback_to_local": True,
                "alibaba": {
                    "endpoint": "wss://dashscope.aliyuncs.com/api-ws/v1/realtime",
                    "api_key": "",
                    "model": "qwen3-asr-flash-realtime-2026-02-10",
                    "turn_mode": "server-vad",
                    "vad_threshold": 0.0,
                    "silence_duration_ms": 400,
                    "final_pass_enabled": False,
                    "final_pass_base_url": "",
                    "final_pass_model": "qwen3-asr-flash-2026-02-10",
                    "final_pass_timeout_ms": 20000,
                    "final_pass_enable_itn": False,
                },
            },
            "llm": {
                "enabled": False,
                "api_base_url": "https://api.openai.com/v1",
                "api_key": "",
                "model": "",
                "provider_sort": "",
                "agent_context_enabled": False,
                "agent_context_max_chars": 6000,
            },
            "output": {
                "mode": "type",
                "paste_keys": "shift+Insert",
                "prefer_paste_for_xwayland": True,
                "xwayland_paste_keys": "shift+Insert",
                "pre_type_delay_ms": 140,
                "type_delay_ms": 0,
            },
            "hud": {
                "enabled": True,
                "margin_bottom": 72,
                "height": 56,
                "position": "bottom-center",
                "offset_x": 0,
                "offset_y": 0,
                "nudge_step": 24,
            },
        }

        if not self.config_path.exists():
            return defaults
        try:
            loaded = tomllib.loads(self.config_path.read_text(encoding="utf-8"))
            for section, values in defaults.items():
                loaded.setdefault(section, {})
                for key, value in values.items():
                    if isinstance(value, dict):
                        loaded[section].setdefault(key, {})
                        for nested_key, nested_value in value.items():
                            loaded[section][key].setdefault(nested_key, nested_value)
                    else:
                        loaded[section].setdefault(key, value)
            return loaded
        except Exception:
            return defaults

    def make_entry(self, group: Adw.PreferencesGroup, title: str, value: str) -> Adw.EntryRow:
        row = Adw.EntryRow(title=title)
        row.set_text(value)
        group.add(row)
        return row

    def make_password_entry(self, group: Adw.PreferencesGroup, title: str) -> Adw.PasswordEntryRow:
        row = Adw.PasswordEntryRow(title=title)
        row.set_text("")
        row.add_css_class("monospace")
        group.add(row)
        return row

    def credential_path(self, credential_id: str) -> Path:
        config_home = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
        return config_home / "credstore.encrypted" / credential_id

    def credential_title(self, label: str, credential_id: str) -> str:
        state = "configured; leave blank to keep" if self.credential_path(credential_id).exists() else "not configured"
        return f"{label} ({state})"

    def update_credential(self, credential_id: str, value: str) -> None:
        if not value:
            return
        directory = self.credential_path(credential_id).parent
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(directory, 0o700)
        fd, temporary = tempfile.mkstemp(prefix=f".{credential_id}.", dir=directory)
        os.close(fd)
        os.unlink(temporary)
        try:
            result = subprocess.run(
                [
                    "systemd-creds",
                    "encrypt",
                    "--user",
                    f"--name={credential_id}",
                    "-",
                    temporary,
                ],
                input=(value + "\n").encode(),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if result.returncode != 0:
                raise RuntimeError(result.stderr.decode(errors="replace").strip())
            os.chmod(temporary, 0o600)
            os.replace(temporary, self.credential_path(credential_id))
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def read_credential(self, credential_id: str) -> str:
        path = self.credential_path(credential_id)
        if not path.exists():
            return ""
        result = subprocess.run(
            ["systemd-creds", "decrypt", "--user", f"--name={credential_id}", str(path), "-"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(result.stderr.decode(errors="replace").strip())
        return result.stdout.decode().rstrip("\r\n")

    def make_combo(self, group: Adw.PreferencesGroup, title: str, values, current: str) -> Adw.ComboRow:
        model = Gtk.StringList.new([label for label, _value in values])
        row = Adw.ComboRow(title=title, model=model)
        row.set_selected(next((i for i, (_, v) in enumerate(values) if v == current), 0))
        group.add(row)
        return row

    def collect(self) -> dict:
        language_value = LANGUAGE_VALUES[self.language_combo.get_selected()][1]
        mode_value = HOTKEY_MODES[self.mode_combo.get_selected()][1]
        provider_value = PROVIDER_VALUES[self.provider_combo.get_selected()][1]
        turn_mode_value = TURN_MODE_VALUES[self.alibaba_turn_mode.get_selected()][1]
        hud_position_value = HUD_POSITION_VALUES[self.hud_position.get_selected()][1]
        return {
            "state_file": "auto",
            "hotkey": {
                "accelerator": self.hotkey_entry.get_text(),
                "mode": mode_value,
            },
            "audio": {
                "device": "default",
                "sample_rate": 16000,
                "max_duration_secs": 90,
                "partial_interval_ms": 1500,
                "pre_roll_enabled": self.pre_roll_switch.get_active(),
                "pre_roll_ms": int(self.pre_roll_ms.get_text() or "500"),
            },
            "asr": {
                "provider": provider_value,
                "backend_command": self.backend_command.get_text(),
                "engine": self.engine_entry.get_text(),
                "model": self.model_entry.get_text(),
                "language": language_value,
                "connect_timeout_ms": int(self.connect_timeout.get_text() or "5000"),
                "finalize_timeout_ms": int(self.finalize_timeout.get_text() or "8000"),
                "fallback_to_local": self.local_fallback.get_active(),
                "alibaba": {
                    "endpoint": self.alibaba_endpoint.get_text(),
                    "api_key": self.alibaba_api_key.get_text(),
                    "model": self.alibaba_model.get_text(),
                    "turn_mode": turn_mode_value,
                    "vad_threshold": float(self.alibaba_vad_threshold.get_text() or "0"),
                    "silence_duration_ms": int(self.alibaba_silence_ms.get_text() or "400"),
                    "final_pass_enabled": self.alibaba_final_pass.get_active(),
                    "final_pass_base_url": self.alibaba_final_base_url.get_text(),
                    "final_pass_model": self.alibaba_final_model.get_text(),
                    "final_pass_timeout_ms": int(
                        self.alibaba_final_timeout.get_text() or "20000"
                    ),
                    "final_pass_enable_itn": self.alibaba_final_itn.get_active(),
                },
            },
            "output": {
                "mode": self.output_mode.get_text() or "type",
                "fallback_to_clipboard": True,
                "type_delay_ms": int(self.type_delay.get_text() or "0"),
                "pre_type_delay_ms": int(self.pre_delay.get_text() or "140"),
                "paste_keys": self.paste_keys.get_text() or "shift+Insert",
                "prefer_paste_for_xwayland": self.xwayland_paste_switch.get_active(),
                "xwayland_paste_keys": self.xwayland_paste_keys.get_text() or "shift+Insert",
            },
            "ime": {
                "manage_fcitx5": True,
                "force_ascii_before_output": True,
            },
            "llm": {
                "enabled": self.llm_switch.get_active(),
                "api_base_url": self.api_base_entry.get_text(),
                "api_key": self.api_key_entry.get_text(),
                "model": self.model_llm_entry.get_text(),
                "timeout_ms": 5000,
                "provider_sort": self.provider_sort_entry.get_text(),
                "agent_context_enabled": self.agent_context_switch.get_active(),
                "agent_context_max_chars": int(
                    self.agent_context_max_chars.get_text() or "6000"
                ),
            },
            "hud": {
                "enabled": True,
                "margin_bottom": int(self.hud_margin_bottom.get_text() or "72"),
                "height": int(self.hud_height.get_text() or "56"),
                "position": hud_position_value,
                "offset_x": int(self.hud_offset_x.get_text() or "0"),
                "offset_y": int(self.hud_offset_y.get_text() or "0"),
                "nudge_step": int(self.hud_nudge_step.get_text() or "24"),
            },
        }

    def render_toml(self, data: dict) -> str:
        return f"""state_file = "{escape_toml(data['state_file'])}"

[hotkey]
accelerator = "{escape_toml(data['hotkey']['accelerator'])}"
mode = "{escape_toml(data['hotkey']['mode'])}"

[audio]
device = "{escape_toml(data['audio']['device'])}"
sample_rate = {data['audio']['sample_rate']}
max_duration_secs = {data['audio']['max_duration_secs']}
partial_interval_ms = {data['audio']['partial_interval_ms']}
pre_roll_enabled = {str(data['audio']['pre_roll_enabled']).lower()}
pre_roll_ms = {data['audio']['pre_roll_ms']}

[asr]
provider = "{escape_toml(data['asr']['provider'])}"
backend_command = "{escape_toml(data['asr']['backend_command'])}"
engine = "{escape_toml(data['asr']['engine'])}"
model = "{escape_toml(data['asr']['model'])}"
language = "{escape_toml(data['asr']['language'])}"
connect_timeout_ms = {data['asr']['connect_timeout_ms']}
finalize_timeout_ms = {data['asr']['finalize_timeout_ms']}
fallback_to_local = {str(data['asr']['fallback_to_local']).lower()}

[asr.alibaba]
endpoint = "{escape_toml(data['asr']['alibaba']['endpoint'])}"
model = "{escape_toml(data['asr']['alibaba']['model'])}"
turn_mode = "{escape_toml(data['asr']['alibaba']['turn_mode'])}"
vad_threshold = {data['asr']['alibaba']['vad_threshold']}
silence_duration_ms = {data['asr']['alibaba']['silence_duration_ms']}
final_pass_enabled = {str(data['asr']['alibaba']['final_pass_enabled']).lower()}
final_pass_base_url = "{escape_toml(data['asr']['alibaba']['final_pass_base_url'])}"
final_pass_model = "{escape_toml(data['asr']['alibaba']['final_pass_model'])}"
final_pass_timeout_ms = {data['asr']['alibaba']['final_pass_timeout_ms']}
final_pass_enable_itn = {str(data['asr']['alibaba']['final_pass_enable_itn']).lower()}

[output]
mode = "{escape_toml(data['output']['mode'])}"
fallback_to_clipboard = true
type_delay_ms = {data['output']['type_delay_ms']}
pre_type_delay_ms = {data['output']['pre_type_delay_ms']}
paste_keys = "{escape_toml(data['output']['paste_keys'])}"
prefer_paste_for_xwayland = {str(data['output']['prefer_paste_for_xwayland']).lower()}
xwayland_paste_keys = "{escape_toml(data['output']['xwayland_paste_keys'])}"

[ime]
manage_fcitx5 = {str(data['ime']['manage_fcitx5']).lower()}
force_ascii_before_output = {str(data['ime']['force_ascii_before_output']).lower()}

[llm]
enabled = {str(data['llm']['enabled']).lower()}
api_base_url = "{escape_toml(data['llm']['api_base_url'])}"
model = "{escape_toml(data['llm']['model'])}"
timeout_ms = {data['llm']['timeout_ms']}
provider_sort = "{escape_toml(data['llm']['provider_sort'])}"
agent_context_enabled = {str(data['llm']['agent_context_enabled']).lower()}
agent_context_max_chars = {data['llm']['agent_context_max_chars']}

[hud]
enabled = {str(data['hud']['enabled']).lower()}
margin_bottom = {data['hud']['margin_bottom']}
height = {data['hud']['height']}
position = "{escape_toml(data['hud']['position'])}"
offset_x = {data['hud']['offset_x']}
offset_y = {data['hud']['offset_y']}
nudge_step = {data['hud']['nudge_step']}
"""

    def on_save_clicked(self, *_args) -> None:
        data = self.collect()
        try:
            self.update_credential("alibaba-api-key", data["asr"]["alibaba"]["api_key"])
            self.update_credential("openrouter-api-key", data["llm"]["api_key"])
            self.config_path.parent.mkdir(parents=True, exist_ok=True)
            self.config_path.write_text(self.render_toml(data), encoding="utf-8")
            os.chmod(self.config_path, 0o600)
            subprocess.run(
                ["systemctl", "--user", "restart", "voice-input.service"],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            self.alibaba_api_key.set_text("")
            self.api_key_entry.set_text("")
            self.status.set_label("Configuration and credentials saved; service restarted")
        except Exception as error:  # noqa: BLE001
            self.status.set_label(f"Save failed: {error}")

    def on_test_clicked(self, *_args) -> None:
        data = self.collect()["llm"]
        if not data["enabled"]:
            self.status.set_label("LLM refinement is disabled")
            return
        try:
            api_key = data["api_key"] or self.read_credential("openrouter-api-key")
        except Exception as error:  # noqa: BLE001
            self.status.set_label(f"Credential read failed: {error}")
            return
        if not api_key or not data["model"]:
            self.status.set_label("OpenRouter credential and model are required for Test")
            return

        payload = {
            "model": data["model"],
            "temperature": 0,
            "messages": [
                {
                    "role": "system",
                    "content": "Reply with OK only.",
                },
                {
                    "role": "user",
                    "content": "Connectivity test.",
                },
            ],
        }
        request = urllib.request.Request(
            data["api_base_url"].rstrip("/") + "/chat/completions",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {api_key}",
            },
            data=json.dumps(payload).encode("utf-8"),
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=7) as response:
                if response.status == 200:
                    self.status.set_label("LLM connectivity OK")
                else:
                    self.status.set_label(f"LLM test failed with HTTP {response.status}")
        except urllib.error.HTTPError as error:
            self.status.set_label(f"LLM test failed with HTTP {error.code}")
        except Exception as error:  # noqa: BLE001
            self.status.set_label(f"LLM test failed: {error}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", required=True)
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()

    app = Adw.Application(application_id="org.omarchy.voice-input.settings")

    def activate(application: Adw.Application) -> None:
        window = SettingsWindow(Path(args.config), Path(args.binary))
        window.set_application(application)
        window.present()

    app.connect("activate", activate)
    app.run(None)


if __name__ == "__main__":
    main()
