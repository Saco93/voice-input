#!/usr/bin/env python3

import argparse
import json
import math
import os
import socket
import subprocess
import time
from pathlib import Path

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
gi.require_version("Gtk4LayerShell", "1.0")

from gi.repository import Gdk, GLib, Gtk, Gtk4LayerShell, Pango, cairo  # noqa: E402


CSS = """
window#hud-window {
  background: transparent;
  box-shadow: none;
}

#hud-root {
  background: transparent;
}

#hud-capsule {
  background: rgba(17, 20, 27, 0.88);
  border-radius: 28px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  color: #eef2ff;
  padding: 10px 16px;
  box-shadow: 0 18px 50px rgba(0, 0, 0, 0.28);
}

#hud-transcript {
  font-size: 14px;
  font-weight: 600;
}
"""

COMPACT_WIDTH = 400
EXPANDED_WIDTH = 620
WAVEFORM_BAR_COUNT = 30
HUD_EDGE_MARGIN = 24


class HudWindow(Gtk.Window):
    def __init__(
        self,
        state_file: Path,
        waveform_socket: Path,
        height: int,
        margin_bottom: int,
    ) -> None:
        super().__init__(title="Voice Input HUD")
        self.state_file = state_file
        self.waveform_socket_path = waveform_socket
        self.waveform_socket = None
        self.waveform_buffer = b""
        self.waveform_target = [0.0] * WAVEFORM_BAR_COUNT
        self.waveform_retry_at = 0.0
        self.base_height = height
        self.margin_bottom = margin_bottom
        self.current_width = COMPACT_WIDTH
        self.target_width = COMPACT_WIDTH
        self.current_height = height
        self.target_height = height
        self.current_opacity = 0.0
        self.target_opacity = 0.0
        self.current_bars = [0.0] * WAVEFORM_BAR_COUNT
        self.phase = "idle"
        self.last_payload = {}
        self.last_anchor = None
        self.last_size = None
        self.last_label_width = None
        self.last_reanchor_at = 0.0
        self.last_display_text = ""
        self.cached_client_address = None
        self.pending_reanchor = None
        self.pending_reanchor_at = 0.0
        self.debug_log_path = self.resolve_debug_log_path()
        self.uses_layer_shell = False
        self.hud_position = "bottom-center"
        self.hud_offset_x = 0
        self.hud_offset_y = 0

        self.debug(
            "startup "
            f"wayland={os.environ.get('WAYLAND_DISPLAY', '')!r} "
            f"hypr_sig={bool(os.environ.get('HYPRLAND_INSTANCE_SIGNATURE'))} "
            f"ld_preload={os.environ.get('LD_PRELOAD', '')!r} "
            f"supported={Gtk4LayerShell.is_supported()}"
        )

        self.set_name("hud-window")
        self.set_decorated(False)
        self.set_resizable(False)
        self.set_focusable(False)
        self.set_can_focus(False)
        self.set_focus_on_click(False)
        self.set_can_target(False)
        self.set_auto_startup_notification(False)
        self.set_startup_id("")
        self.set_default_size(self.current_width, height)
        self.set_size_request(self.current_width, height)
        self.connect("realize", self.on_realize)

        Gtk4LayerShell.init_for_window(self)
        Gtk4LayerShell.set_layer(self, Gtk4LayerShell.Layer.OVERLAY)
        Gtk4LayerShell.set_keyboard_mode(self, Gtk4LayerShell.KeyboardMode.NONE)
        Gtk4LayerShell.set_namespace(self, "voice-input-hud")
        Gtk4LayerShell.set_anchor(self, Gtk4LayerShell.Edge.LEFT, True)
        Gtk4LayerShell.set_anchor(self, Gtk4LayerShell.Edge.RIGHT, False)
        Gtk4LayerShell.set_anchor(self, Gtk4LayerShell.Edge.BOTTOM, True)
        Gtk4LayerShell.set_margin(self, Gtk4LayerShell.Edge.LEFT, 0)
        Gtk4LayerShell.set_margin(self, Gtk4LayerShell.Edge.BOTTOM, margin_bottom)
        self.uses_layer_shell = Gtk4LayerShell.is_layer_window(self)
        self.debug(
            "layershell "
            f"is_layer_window={Gtk4LayerShell.is_layer_window(self)} "
            f"namespace={Gtk4LayerShell.get_namespace(self)!r} "
            f"layer={int(Gtk4LayerShell.get_layer(self))}"
        )

        provider = Gtk.CssProvider()
        provider.load_from_data(CSS.encode())
        Gtk.StyleContext.add_provider_for_display(
            Gdk.Display.get_default(),
            provider,
            Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION,
        )

        root = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        root.set_name("hud-root")
        root.set_hexpand(True)
        root.set_halign(Gtk.Align.FILL)
        root.set_valign(Gtk.Align.END)
        root.set_margin_bottom(0)
        root.set_can_target(False)

        self.capsule = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=12)
        self.capsule.set_name("hud-capsule")
        self.capsule.set_halign(Gtk.Align.CENTER)
        self.capsule.set_valign(Gtk.Align.CENTER)
        self.capsule.set_size_request(self.current_width, height)
        self.capsule.set_opacity(0.0)
        self.capsule.set_can_target(False)

        self.wave = Gtk.DrawingArea()
        self.wave.set_content_width(190)
        self.wave.set_content_height(32)
        self.wave.set_draw_func(self.draw_wave)
        self.wave.set_can_target(False)

        label_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        label_box.set_size_request(160, -1)
        label_box.set_halign(Gtk.Align.FILL)
        label_box.set_valign(Gtk.Align.CENTER)
        label_box.set_hexpand(True)
        label_box.set_can_target(False)

        self.label = Gtk.Label()
        self.label.set_name("hud-transcript")
        self.label.set_xalign(0.0)
        self.label.set_yalign(0.5)
        self.label.set_ellipsize(Pango.EllipsizeMode.NONE)
        self.label.set_wrap(True)
        self.label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        self.label.set_single_line_mode(False)
        self.label.set_lines(0)
        self.label.set_max_width_chars(0)
        self.label.set_natural_wrap_mode(Gtk.NaturalWrapMode.WORD)
        self.label.set_label("Listening…")
        self.label.set_hexpand(True)
        self.label.set_halign(Gtk.Align.FILL)
        self.label.set_valign(Gtk.Align.CENTER)
        self.label.set_can_target(False)

        label_box.append(self.label)
        self.capsule.append(self.wave)
        self.capsule.append(label_box)
        root.append(self.capsule)
        self.set_child(root)

        GLib.timeout_add(33, self.tick)

    def draw_wave(self, area: Gtk.DrawingArea, cr: cairo.Context, width: int, height: int) -> None:
        bar_width = 4
        gap = 2
        total_width = WAVEFORM_BAR_COUNT * bar_width + (WAVEFORM_BAR_COUNT - 1) * gap
        offset_x = max((width - total_width) / 2, 0)
        bottom = height - 2
        cr.set_source_rgba(0.77, 0.87, 1.0, 0.96)

        for index, value in enumerate(self.current_bars):
            level = max(0.0, min(1.0, value))
            bar_height = 0 if level <= 0.001 else 3 + level * 31
            if bar_height == 0:
                continue
            x = offset_x + index * (bar_width + gap)
            y = bottom - bar_height
            radius = bar_width / 2
            self.rounded_rect(cr, x, y, bar_width, bar_height, radius)
            cr.fill()

    def rounded_rect(self, cr: cairo.Context, x: float, y: float, w: float, h: float, r: float) -> None:
        cr.new_sub_path()
        cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
        cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
        cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
        cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
        cr.close_path()

    def poll_waveform(self) -> None:
        now = time.monotonic()
        if self.waveform_socket is None and now >= self.waveform_retry_at:
            candidate = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            candidate.settimeout(0.05)
            try:
                candidate.connect(str(self.waveform_socket_path))
                candidate.setblocking(False)
                self.waveform_socket = candidate
                self.waveform_buffer = b""
            except OSError:
                candidate.close()
                self.waveform_retry_at = now + 0.4

        if self.waveform_socket is None:
            return

        try:
            while True:
                chunk = self.waveform_socket.recv(4096)
                if not chunk:
                    raise ConnectionError("waveform socket closed")
                self.waveform_buffer += chunk
        except BlockingIOError:
            pass
        except OSError:
            self.waveform_socket.close()
            self.waveform_socket = None
            self.waveform_buffer = b""
            self.waveform_target = [0.0] * WAVEFORM_BAR_COUNT
            self.waveform_retry_at = now + 0.4
            return

        lines = self.waveform_buffer.split(b"\n")
        self.waveform_buffer = lines.pop()
        for line in lines:
            try:
                message = json.loads(line)
                if message.get("type") == "reset":
                    self.waveform_target = [0.0] * WAVEFORM_BAR_COUNT
                elif message.get("type") == "waveform":
                    bars = message.get("bars")
                    if isinstance(bars, list) and len(bars) == WAVEFORM_BAR_COUNT:
                        values = [max(0.0, min(1.0, float(value))) for value in bars]
                        if all(math.isfinite(value) for value in values):
                            self.waveform_target = values
            except (TypeError, ValueError, json.JSONDecodeError):
                continue

    def tick(self) -> bool:
        self.poll_waveform()
        payload = self.load_state()
        if payload:
            self.last_payload = payload
            self.phase = payload.get("phase", "idle")
            transcript = (payload.get("transcript") or "").strip()
            tooltip = (payload.get("tooltip") or "").strip()
            status_text = {
                "arming": "Arming microphone…",
                "recording": "Listening…",
                "transcribing": "Transcribing…",
                "refining": "Refining transcript…",
                "outputting": "Sending text…",
                "error": payload.get("error") or "Voice input error",
            }.get(self.phase, "")
            if transcript:
                display_text = transcript
            elif self.phase == "arming" and tooltip:
                display_text = tooltip
            else:
                display_text = status_text
            if display_text != self.last_display_text:
                self.label.set_label(display_text)
                self.last_display_text = display_text

            self.hud_position = payload.get("hud_position", "bottom-center")
            self.hud_offset_x = self.coerce_int(payload.get("hud_offset_x"), 0)
            self.hud_offset_y = self.coerce_int(payload.get("hud_offset_y"), 0)
            self.target_width = self.compute_target_width(transcript, display_text)
            self.target_opacity = 1.0 if self.phase != "idle" else 0.0
            available_width = max(160, int(self.target_width) - 250)
            if self.last_label_width != available_width:
                self.label.set_size_request(available_width, -1)
                self.last_label_width = available_width
            label_box_height = self.measure_label_height(available_width)
            self.target_height = max(self.base_height, label_box_height + 24)
            targets = (
                self.waveform_target
                if self.phase in ("arming", "recording")
                else payload.get("bars", [0.0] * WAVEFORM_BAR_COUNT)
            )
            self.current_bars = [
                bar + (target - bar) * 0.45
                for bar, target in zip(self.current_bars, targets)
            ]
            self.wave.queue_draw()

        self.current_opacity += (self.target_opacity - self.current_opacity) * 0.22
        width = int(self.target_width)
        height = int(self.target_height)
        self.current_width = width
        self.current_height = height
        self.wave.set_content_height(max(32, height - 24))
        reanchor_needed = False
        if self.last_size != (width, height):
            self.capsule.set_size_request(width, height)
            self.set_default_size(width, height)
            self.set_size_request(width, height)
            self.last_size = (width, height)
            reanchor_needed = True
        opacity = max(0.0, min(1.0, self.current_opacity))
        self.capsule.set_opacity(opacity)
        self.set_opacity(opacity)
        if self.uses_layer_shell:
            if reanchor_needed or self.phase != "idle":
                self.apply_layer_position(width)
        else:
            if reanchor_needed:
                self.queue_reanchor(width, height, delay=0.05)
            elif self.phase != "idle" and (self.last_anchor is None or self.cached_client_address is None):
                self.queue_reanchor(width, height, delay=0.02)
            self.maybe_reanchor()
        if self.phase == "idle" and opacity < 0.03:
            self.close()
            return False
        return True

    def measure_label_height(self, width: int) -> int:
        _minimum, natural, _minimum_baseline, _natural_baseline = self.label.measure(
            Gtk.Orientation.VERTICAL,
            width,
        )
        return max(32, natural)

    def compute_target_width(self, transcript: str, display_text: str) -> int:
        if transcript or len(display_text) > 18 or self.phase in {"transcribing", "refining", "outputting", "error"}:
            return EXPANDED_WIDTH
        return COMPACT_WIDTH

    def apply_layer_position(self, width: int) -> None:
        margins = self.compute_layer_margins(width)
        if margins is None:
            self.debug("layer_position_skipped margins=None")
            return

        left_margin, bottom_margin = margins
        Gtk4LayerShell.set_margin(self, Gtk4LayerShell.Edge.LEFT, left_margin)
        Gtk4LayerShell.set_margin(self, Gtk4LayerShell.Edge.BOTTOM, bottom_margin)
        self.debug(
            f"layer_position left_margin={left_margin} bottom_margin={bottom_margin} width={width}"
        )

    def queue_reanchor(self, width: int, height: int, delay: float = 0.05) -> None:
        self.pending_reanchor = (width, height)
        self.pending_reanchor_at = time.monotonic() + max(0.0, delay)
        self.debug(
            f"queue_reanchor width={width} height={height} delay={delay:.2f} phase={self.phase}"
        )

    def maybe_reanchor(self) -> None:
        if self.pending_reanchor is None:
            return
        now = time.monotonic()
        if now < self.pending_reanchor_at:
            return

        width, height = self.pending_reanchor
        if self.reanchor_window(width, height):
            self.pending_reanchor = None
            return

        # The client can appear in Hyprland a little after GTK maps the window.
        self.pending_reanchor_at = now + 0.05
        self.debug(f"reanchor_retry_scheduled width={width} height={height}")

    def reanchor_window(self, width: int, height: int) -> bool:
        if self.phase == "idle":
            self.last_anchor = None
            self.last_reanchor_at = 0.0
            self.cached_client_address = None
            self.pending_reanchor = None
            return True

        anchor = self.compute_anchor(width, height)
        if anchor is None:
            self.debug("reanchor_skipped anchor=None")
            return False
        if anchor == self.last_anchor and self.cached_client_address:
            self.debug(f"reanchor_skipped anchor_unchanged={anchor}")
            return True
        now = time.monotonic()
        if now - self.last_reanchor_at < 0.08:
            self.debug("reanchor_skipped throttle")
            return False

        client_address = self.cached_client_address or self.lookup_client_address()
        if not client_address:
            self.debug("reanchor_skipped client_address=None")
            return False
        self.cached_client_address = client_address

        x, y = anchor
        try:
            subprocess.run(
                [
                    "hyprctl",
                    "dispatch",
                    "movewindowpixel",
                    f"exact {x} {y},address:{client_address}",
                ],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.last_anchor = anchor
            self.last_reanchor_at = now
            self.debug(
                f"reanchor_ok address={client_address} anchor={anchor}"
            )
            return True
        except Exception as exc:
            self.cached_client_address = None
            self.debug(
                f"reanchor_failed address={client_address} anchor={anchor} error={exc!r}"
            )
            return False

    def compute_anchor(self, width: int, height: int) -> tuple[int, int] | None:
        geometry = self.current_monitor_geometry()
        if geometry is None:
            return None

        monitor_x = geometry["monitor_x"]
        monitor_y = geometry["monitor_y"]
        left = geometry["left"]
        top = geometry["top"]
        usable_width = geometry["usable_width"]
        usable_height = geometry["usable_height"]

        base_left = self.base_left_margin(usable_width, width)
        max_left = max(0, usable_width - width)
        left_margin = max(0, min(max_left, base_left + self.hud_offset_x))
        bottom_margin = max(0, self.margin_bottom + self.hud_offset_y)

        x = monitor_x + left + left_margin
        y = monitor_y + top + max(0, usable_height - height - bottom_margin)
        return (x, y)

    def compute_layer_margins(self, width: int) -> tuple[int, int] | None:
        geometry = self.current_monitor_geometry()
        if geometry is None:
            return None

        left = geometry["left"]
        bottom = geometry["bottom"]
        usable_width = geometry["usable_width"]
        base_left = self.base_left_margin(usable_width, width)
        max_left = max(0, usable_width - width)
        left_margin = left + max(0, min(max_left, base_left + self.hud_offset_x))
        bottom_margin = max(0, bottom + self.margin_bottom + self.hud_offset_y)
        return (left_margin, bottom_margin)

    def base_left_margin(self, usable_width: int, width: int) -> int:
        if self.hud_position == "bottom-left":
            return HUD_EDGE_MARGIN
        if self.hud_position == "bottom-right":
            return max(0, usable_width - width - HUD_EDGE_MARGIN)
        return max(0, (usable_width - width) // 2)

    def current_monitor_geometry(self) -> dict | None:
        try:
            output = subprocess.check_output(
                ["hyprctl", "monitors", "-j"],
                text=True,
                stderr=subprocess.DEVNULL,
            )
            monitors = json.loads(output)
        except Exception:
            return None

        if not monitors:
            return None

        monitor = next((item for item in monitors if item.get("focused")), monitors[0])
        scale = float(monitor.get("scale") or 1.0)
        logical_width = int(round(float(monitor.get("width", 0)) / scale))
        logical_height = int(round(float(monitor.get("height", 0)) / scale))
        monitor_x = int(monitor.get("x", 0))
        monitor_y = int(monitor.get("y", 0))
        reserved = monitor.get("reserved") or [0, 0, 0, 0]
        left = int(reserved[0]) if len(reserved) > 0 else 0
        top = int(reserved[1]) if len(reserved) > 1 else 0
        right = int(reserved[2]) if len(reserved) > 2 else 0
        bottom = int(reserved[3]) if len(reserved) > 3 else 0

        usable_width = max(0, logical_width - left - right)
        usable_height = max(0, logical_height - top - bottom)
        return {
            "monitor_x": monitor_x,
            "monitor_y": monitor_y,
            "left": left,
            "top": top,
            "bottom": bottom,
            "usable_width": usable_width,
            "usable_height": usable_height,
        }

    def lookup_client_address(self) -> str | None:
        try:
            output = subprocess.check_output(
                ["hyprctl", "clients", "-j"],
                text=True,
                stderr=subprocess.DEVNULL,
            )
            clients = json.loads(output)
        except Exception:
            return None

        own_pid = os.getpid()
        for client in clients:
            if client.get("pid") == own_pid:
                address = client.get("address")
                if isinstance(address, str) and address:
                    self.debug(f"lookup_client_address pid_match address={address}")
                    return address

        for client in clients:
            if client.get("title") == "Voice Input HUD":
                address = client.get("address")
                if isinstance(address, str) and address:
                    self.debug(f"lookup_client_address title_match address={address}")
                    return address
        self.debug("lookup_client_address no_match")
        return None

    def on_realize(self, *_args) -> None:
        surface = self.get_surface()
        layer_surface = Gtk4LayerShell.get_zwlr_layer_surface_v1(self)
        self.uses_layer_shell = Gtk4LayerShell.is_layer_window(self)
        self.debug(
            "realize "
            f"surface={surface is not None} "
            f"layer_surface={layer_surface is not None} "
            f"is_layer_window={Gtk4LayerShell.is_layer_window(self)}"
        )
        if self.uses_layer_shell:
            self.apply_layer_position(self.current_width)
        else:
            self.queue_reanchor(self.current_width, int(self.current_height), delay=0.06)

    def load_state(self) -> dict:
        try:
            if not self.state_file.exists():
                return {}
            return json.loads(self.state_file.read_text(encoding="utf-8"))
        except Exception:
            return self.last_payload

    def resolve_debug_log_path(self) -> Path:
        runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
        if runtime_dir:
            path = Path(runtime_dir) / "voice-input" / "hud-debug.log"
        else:
            path = Path("/tmp") / "voice-input-hud-debug.log"
        path.parent.mkdir(parents=True, exist_ok=True)
        return path

    def debug(self, message: str) -> None:
        timestamp = time.strftime("%H:%M:%S")
        try:
            with self.debug_log_path.open("a", encoding="utf-8") as handle:
                handle.write(f"[{timestamp}] {message}\n")
        except Exception:
            pass

    def coerce_int(self, value, default: int) -> int:
        try:
            return int(value)
        except (TypeError, ValueError):
            return default


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--state-file", required=True)
    parser.add_argument("--waveform-socket", required=True)
    parser.add_argument("--height", type=int, default=56)
    parser.add_argument("--margin-bottom", type=int, default=72)
    args = parser.parse_args()

    os.environ.setdefault("GSK_RENDERER", "ngl")
    Gtk.init()

    loop = GLib.MainLoop()
    window = HudWindow(
        Path(args.state_file),
        Path(args.waveform_socket),
        args.height,
        args.margin_bottom,
    )

    def on_close_request(*_args) -> bool:
        loop.quit()
        return False

    window.connect("close-request", on_close_request)
    window.show()
    loop.run()


if __name__ == "__main__":
    main()
