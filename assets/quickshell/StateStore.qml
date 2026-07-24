import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    id: root

    readonly property string runtimeDirectory: {
        const configured = Quickshell.env("XDG_RUNTIME_DIR");
        return configured && configured.length > 0 ? configured : "/run/user/1000";
    }
    readonly property string statePath: runtimeDirectory + "/voice-input/state.json"
    readonly property string waveformPath: runtimeDirectory + "/voice-input/waveform.sock"
    readonly property string themePath: Quickshell.env("HOME")
        + "/.config/omarchy/current/theme/colors.toml"
    readonly property int waveformBarCount: 30
    readonly property var emptyWaveform: new Array(waveformBarCount).fill(0)
    property var waveformBars: emptyWaveform
    property int waveformFrameCount: 0
    property var themePalette: ({
        "accent": "#7dd3fc",
        "foreground": "#eef2ff",
        "warning": "#facc15",
        "refining": "#c4b5fd",
        "error": "#fb7185"
    })
    readonly property color themeAccent: themePalette.accent
    readonly property color themeForeground: themePalette.foreground
    readonly property color themeWarning: themePalette.warning
    readonly property color themeRefining: themePalette.refining
    readonly property color themeError: themePalette.error

    property var snapshot: ({
        "phase": "idle",
        "transcript": "",
        "tooltip": "",
        "bars": [],
        "hud_enabled": true,
        "hud_margin_bottom": 72,
        "hud_height": 56,
        "hud_position": "bottom-center",
        "hud_offset_x": 0,
        "hud_offset_y": 0,
        "error": null
    })

    readonly property string phase: snapshot.phase || "idle"
    readonly property string transcript: snapshot.transcript || ""
    readonly property string tooltip: snapshot.tooltip || ""
    readonly property var bars: (phase === "arming" || phase === "recording")
        ? waveformBars
        : (snapshot.bars || emptyWaveform)
    readonly property bool hudEnabled: snapshot.hud_enabled === undefined
        ? true : Boolean(snapshot.hud_enabled)
    readonly property int hudMarginBottom: snapshot.hud_margin_bottom === undefined
        ? 72 : Number(snapshot.hud_margin_bottom)
    readonly property int hudHeight: snapshot.hud_height === undefined
        ? 56 : Number(snapshot.hud_height)
    readonly property string hudPosition: snapshot.hud_position || "bottom-center"
    readonly property int hudOffsetX: snapshot.hud_offset_x || 0
    readonly property int hudOffsetY: snapshot.hud_offset_y || 0
    readonly property string errorText: snapshot.error || ""
    readonly property bool active: phase !== "idle"

    function refreshSnapshot() {
        try {
            stateFile.reload();
            const parsed = JSON.parse(stateFile.text());
            if (parsed && parsed.phase && parsed.updated_at_ms !== snapshot.updated_at_ms) {
                const previousPhase = snapshot.phase;
                snapshot = parsed;
                if (parsed.phase !== previousPhase) {
                    console.info("Voice Input HUD state:", previousPhase, "->", parsed.phase);
                    if (parsed.phase === "idle")
                        resetWaveform();
                }
            }
        } catch (error) {
            // Keep the last valid snapshot and retry on the next timer tick.
        }
    }

    function themeColor(source, key, fallback) {
        const expression = new RegExp("^\\s*" + key
            + "\\s*=\\s*[\\\"'](#[0-9a-fA-F]{6,8})[\\\"']", "m");
        const match = source.match(expression);
        return match ? match[1] : fallback;
    }

    function refreshTheme() {
        try {
            themeFile.reload();
            const source = themeFile.text();
            const next = {
                "accent": themeColor(source, "accent", themePalette.accent),
                "foreground": themeColor(source, "foreground", themePalette.foreground),
                "warning": themeColor(source, "color3", themePalette.warning),
                "refining": themeColor(source, "color5", themePalette.refining),
                "error": themeColor(source, "color1", themePalette.error)
            };
            if (JSON.stringify(next) !== JSON.stringify(themePalette)) {
                themePalette = next;
                console.info("Voice Input HUD theme accent:", next.accent);
            }
        } catch (error) {
            // Preserve the last valid palette when a theme is being replaced.
        }
    }

    function resetWaveform() {
        waveformBars = emptyWaveform.slice();
        waveformFrameCount = 0;
    }

    function consumeWaveform(data) {
        try {
            const message = JSON.parse(data);
            if (message.type === "reset") {
                resetWaveform();
                return;
            }
            if (message.type !== "waveform" || !Array.isArray(message.bars)
                    || message.bars.length !== waveformBarCount)
                return;
            const next = [];
            for (let index = 0; index < message.bars.length; index++) {
                const value = Number(message.bars[index]);
                if (!Number.isFinite(value))
                    return;
                next.push(Math.max(0.0, Math.min(1.0, value)));
            }
            waveformBars = next;
            waveformFrameCount++;
            if (waveformFrameCount === 30) {
                const minimum = Math.min(...next).toFixed(3);
                const maximum = Math.max(...next).toFixed(3);
                console.info("Voice Input HUD waveform connected:", minimum, "..", maximum);
            }
        } catch (error) {
            // Ignore malformed or partial frames. SplitParser preserves framing.
        }
    }

    property var waveformSocket: null

    property Component waveformSocketComponent: Component {
        Socket {
            path: root.waveformPath
            connected: true

            parser: SplitParser {
                splitMarker: "\n"
                onRead: data => root.consumeWaveform(data)
            }

            onConnectionStateChanged: {
                if (!connected) {
                    root.resetWaveform();
                    root.waveformReconnectTimer.restart();
                }
            }
            onError: _error => {
                root.resetWaveform();
                root.waveformReconnectTimer.restart();
            }
        }
    }

    function connectWaveform() {
        if (waveformSocket) {
            waveformSocket.connected = false;
            waveformSocket.destroy();
        }
        waveformSocket = waveformSocketComponent.createObject(root);
    }

    property Timer waveformReconnectTimer: Timer {
        interval: 400
        repeat: false
        onTriggered: root.connectWaveform()
    }

    Component.onCompleted: connectWaveform()

    property FileView themeFile: FileView {
        path: root.themePath
        watchChanges: true
        blockAllReads: true
        printErrors: false
        onFileChanged: root.refreshTheme()
    }

    property Timer themeRefreshTimer: Timer {
        interval: 1000
        repeat: true
        running: true
        triggeredOnStart: true
        onTriggered: root.refreshTheme()
    }

    // Status and transcript remain in the atomic state file. Waveform samples
    // arrive independently over QLocalSocket and never trigger file polling.
    property FileView stateFile: FileView {
        path: root.statePath
        watchChanges: false
        blockAllReads: true
        printErrors: false
    }

    property Timer refreshTimer: Timer {
        interval: 50
        repeat: true
        running: true
        triggeredOnStart: true
        onTriggered: root.refreshSnapshot()
    }
}
