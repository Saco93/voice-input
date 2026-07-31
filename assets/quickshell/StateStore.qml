import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    // Keep the last valid snapshot and retry on the next timer tick.
    // Preserve the last valid palette when a theme is being replaced.
    // Ignore malformed or partial frames. SplitParser preserves framing.

    id: root

    readonly property string runtimeDirectory: {
        const configured = Quickshell.env("XDG_RUNTIME_DIR");
        return configured && configured.length > 0 ? configured : "/run/user/1000";
    }
    readonly property string statePath: runtimeDirectory + "/voice-input/state.json"
    readonly property string waveformPath: runtimeDirectory + "/voice-input/waveform.sock"
    readonly property string themePath: Quickshell.env("HOME") + "/.config/omarchy/current/theme/colors.toml"
    readonly property int waveformBarCount: 30
    readonly property int spectrumBandCount: 12
    readonly property int maximumTranscriptLength: 1024 * 1024
    readonly property var snapshotPhases: ["idle", "arming", "recording", "transcribing", "refining", "outputting", "error"]
    readonly property var hudPositions: ["bottom-center", "bottom-left", "bottom-right"]
    readonly property var emptyWaveform: new Array(waveformBarCount).fill(0)
    readonly property var emptySpectrum: new Array(spectrumBandCount).fill(0)
    property var waveformBars: emptyWaveform
    property var voiceSpectrum: emptySpectrum
    property vector4d voiceSpectrum0: Qt.vector4d(0, 0, 0, 0)
    property vector4d voiceSpectrum1: Qt.vector4d(0, 0, 0, 0)
    property vector4d voiceSpectrum2: Qt.vector4d(0, 0, 0, 0)
    property real voiceSpectralFlux: 0
    property real voiceSpectralCentroid: 0.5
    property real voiceSpeechPace: 0
    // Aggregate voice metrics published alongside the bars. The glow-style HUD
    // visualization is driven by these; level falls back to the bar average
    // when an older daemon does not send it.
    property real voiceLevel: 0
    // Fundamental-frequency estimate, 0..1 (deep .. high). Neutral default
    // keeps the breathing rate steady until the first voiced frame arrives.
    property real voicePitch: 0.35
    // Brightness estimate, 0..1 (muffled .. bright). Drives the hue shift.
    property real voiceTimbre: 0.5
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
        "recording_started_at_ms": null,
        "recording_duration_ms": 0,
        "error": null
    })
    readonly property string phase: snapshot.phase || "idle"
    readonly property string transcript: snapshot.transcript || ""
    readonly property string tooltip: snapshot.tooltip || ""
    readonly property var bars: (phase === "arming" || phase === "recording") ? waveformBars : (snapshot.bars || emptyWaveform)
    readonly property bool hudEnabled: snapshot.hud_enabled === undefined ? true : Boolean(snapshot.hud_enabled)
    readonly property int hudMarginBottom: snapshot.hud_margin_bottom === undefined ? 72 : Number(snapshot.hud_margin_bottom)
    readonly property int hudHeight: snapshot.hud_height === undefined ? 56 : Number(snapshot.hud_height)
    readonly property string hudPosition: snapshot.hud_position || "bottom-center"
    readonly property int hudOffsetX: snapshot.hud_offset_x || 0
    readonly property int hudOffsetY: snapshot.hud_offset_y || 0
    readonly property real recordingStartedAtMs: snapshot.recording_started_at_ms === null || snapshot.recording_started_at_ms === undefined ? 0 : Number(snapshot.recording_started_at_ms)
    readonly property real recordingDurationMs: snapshot.recording_duration_ms === undefined ? 0 : Number(snapshot.recording_duration_ms)
    readonly property string errorText: snapshot.error || ""
    readonly property bool active: phase !== "idle"
    property var waveformSocket: null
    property Component waveformSocketComponent
    property Timer waveformReconnectTimer
    property FileView themeFile
    property Timer themeRefreshTimer
    // Status and transcript remain in the atomically replaced state file.
    // FileView change notifications do not reliably follow inode replacement,
    // so use bounded polling: lower frequency while idle and the original
    // responsive cadence while active. Waveform samples use the socket.
    property FileView stateFile
    property Timer refreshTimer
    property int invalidSnapshotCount: 0

    function boundedString(value, maximumLength) {
        return typeof value === "string" && value.length <= maximumLength;
    }

    function optionalString(value, maximumLength) {
        return value === undefined || value === null || boundedString(value, maximumLength);
    }

    function optionalBoolean(value) {
        return value === undefined || value === null || typeof value === "boolean";
    }

    function optionalBoundedInteger(value, minimum, maximum) {
        return value === undefined || Number.isInteger(value) && value >= minimum && value <= maximum;
    }

    function optionalNullableBoundedInteger(value, maximum) {
        return value === undefined || value === null || Number.isSafeInteger(value) && value >= 0 && value <= maximum;
    }

    function snapshotValidationError(candidate) {
        if (!candidate || typeof candidate !== "object" || Array.isArray(candidate))
            return "snapshot is not an object";

        if (typeof candidate.phase !== "string" || snapshotPhases.indexOf(candidate.phase) < 0)
            return "phase is invalid";

        if (!Number.isFinite(candidate.updated_at_ms) || candidate.updated_at_ms < 0)
            return "updated_at_ms is invalid";

        const requiredStrings = [["class", 64], ["icon", 64], ["text", 1024], ["tooltip", maximumTranscriptLength], ["transcript", maximumTranscriptLength], ["language", 256], ["engine", 2048], ["model", 2048]];
        for (let index = 0; index < requiredStrings.length; index++) {
            const field = requiredStrings[index][0];
            if (!boundedString(candidate[field], requiredStrings[index][1]))
                return field + " is invalid";

        }
        if (!Array.isArray(candidate.bars) || candidate.bars.length !== waveformBarCount)
            return "bars are invalid";

        for (let index = 0; index < candidate.bars.length; index++) {
            const value = candidate.bars[index];
            if (!Number.isFinite(value) || value < 0 || value > 1)
                return "bars contain an invalid value";

        }
        if (candidate.hud_enabled !== undefined && typeof candidate.hud_enabled !== "boolean")
            return "hud_enabled is invalid";

        if (!optionalBoundedInteger(candidate.hud_margin_bottom, -10000, 10000))
            return "hud_margin_bottom is invalid";

        if (!optionalBoundedInteger(candidate.hud_height, 16, 1000))
            return "hud_height is invalid";

        if (candidate.hud_position !== undefined && hudPositions.indexOf(candidate.hud_position) < 0)
            return "hud_position is invalid";

        if (!optionalBoundedInteger(candidate.hud_offset_x, -10000, 10000) || !optionalBoundedInteger(candidate.hud_offset_y, -10000, 10000))
            return "HUD offset is invalid";

        if (!optionalNullableBoundedInteger(candidate.recording_started_at_ms, Number.MAX_SAFE_INTEGER) || !optionalNullableBoundedInteger(candidate.recording_duration_ms, 3600000))
            return "recording duration is invalid";

        const optionalStrings = [["raw_transcript", maximumTranscriptLength], ["refined_transcript", maximumTranscriptLength], ["refinement_status", 4096], ["output_target_hint", 256], ["output_target_resolved", 256], ["output_mode", 256], ["output_driver", 256], ["error", 16384]];
        for (let index = 0; index < optionalStrings.length; index++) {
            const field = optionalStrings[index][0];
            if (!optionalString(candidate[field], optionalStrings[index][1]))
                return field + " is invalid";

        }
        if (!optionalBoolean(candidate.refinement_changed))
            return "refinement_changed is invalid";

        return "";
    }

    function reportInvalidSnapshot(reason) {
        invalidSnapshotCount++;
        if (invalidSnapshotCount === 1 || invalidSnapshotCount % 100 === 0)
            console.warn("Voice Input HUD ignored invalid state snapshot:", reason, "count", invalidSnapshotCount);

    }

    function refreshSnapshot() {
        try {
            stateFile.reload();
            const parsed = JSON.parse(stateFile.text());
            const validationError = snapshotValidationError(parsed);
            if (validationError.length > 0) {
                reportInvalidSnapshot(validationError);
                return ;
            }
            if (parsed.updated_at_ms !== snapshot.updated_at_ms) {
                const previousPhase = snapshot.phase;
                snapshot = parsed;
                invalidSnapshotCount = 0;
                if (parsed.phase !== previousPhase) {
                    console.info("Voice Input HUD state:", previousPhase, "->", parsed.phase);
                    if (parsed.phase === "idle")
                        resetWaveform();

                }
            }
        } catch (error) {
            reportInvalidSnapshot("state JSON could not be parsed");
        }
    }

    function themeColor(source, key, fallback) {
        const expression = new RegExp("^\\s*" + key + "\\s*=\\s*[\\\"'](#[0-9a-fA-F]{6,8})[\\\"']", "m");
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
        }
    }

    function resetWaveform() {
        waveformBars = emptyWaveform.slice();
        voiceLevel = 0;
        voicePitch = 0.35;
        voiceTimbre = 0.5;
        voiceSpectrum = emptySpectrum.slice();
        voiceSpectrum0 = Qt.vector4d(0, 0, 0, 0);
        voiceSpectrum1 = Qt.vector4d(0, 0, 0, 0);
        voiceSpectrum2 = Qt.vector4d(0, 0, 0, 0);
        voiceSpectralFlux = 0;
        voiceSpectralCentroid = 0.5;
        voiceSpeechPace = 0;
        waveformFrameCount = 0;
    }

    function consumeWaveform(data) {
        try {
            const message = JSON.parse(data);
            if (message.type === "reset") {
                const resetSessionId = Number(message.session_id);
                // The daemon closes an accepted recording with session_id 0
                // before the polled snapshot can advance to transcribing. Keep
                // the final live frame through that ordering gap so Listening
                // crossfades directly into processing instead of flashing its
                // silent standby envelope. The idle snapshot clears it shortly
                // afterward; a positive ID still resets a newly starting session.
                if (resetSessionId !== 0 || phase === "idle")
                    resetWaveform();

                return ;
            }
            if (message.type !== "waveform" || !Array.isArray(message.bars) || message.bars.length !== waveformBarCount)
                return ;

            const next = [];
            for (let index = 0; index < message.bars.length; index++) {
                const value = Number(message.bars[index]);
                if (!Number.isFinite(value))
                    return ;

                next.push(Math.max(0, Math.min(1, value)));
            }
            waveformBars = next;
            const average = next.reduce((sum, value) => {
                return sum + value;
            }, 0) / next.length;
            const clamp01 = (value) => {
                return Math.max(0, Math.min(1, value));
            };
            const level = Number(message.level);
            const pitch = Number(message.pitch);
            const timbre = Number(message.timbre);
            voiceLevel = Number.isFinite(level) ? clamp01(level) : average;
            voicePitch = Number.isFinite(pitch) ? clamp01(pitch) : 0.35;
            voiceTimbre = Number.isFinite(timbre) ? clamp01(timbre) : 0.5;
            const spectrum = emptySpectrum.slice();
            if (Array.isArray(message.spectrum) && message.spectrum.length === spectrumBandCount) {
                for (let index = 0; index < spectrumBandCount; index++) {
                    const value = Number(message.spectrum[index]);
                    if (Number.isFinite(value))
                        spectrum[index] = clamp01(value);

                }
            }
            voiceSpectrum = spectrum;
            voiceSpectrum0 = Qt.vector4d(spectrum[0], spectrum[1], spectrum[2], spectrum[3]);
            voiceSpectrum1 = Qt.vector4d(spectrum[4], spectrum[5], spectrum[6], spectrum[7]);
            voiceSpectrum2 = Qt.vector4d(spectrum[8], spectrum[9], spectrum[10], spectrum[11]);
            const spectralFlux = Number(message.spectral_flux);
            const spectralCentroid = Number(message.spectral_centroid);
            const speechPace = Number(message.speech_pace);
            voiceSpectralFlux = Number.isFinite(spectralFlux) ? clamp01(spectralFlux) : 0;
            voiceSpectralCentroid = Number.isFinite(spectralCentroid) ? clamp01(spectralCentroid) : 0.5;
            voiceSpeechPace = Number.isFinite(speechPace) ? clamp01(speechPace) : 0;
            waveformFrameCount++;
            if (waveformFrameCount === 30) {
                const minimum = next.reduce((result, value) => {
                    return Math.min(result, value);
                }, 1).toFixed(3);
                const maximum = next.reduce((result, value) => {
                    return Math.max(result, value);
                }, 0).toFixed(3);
                const spectrumMaximum = spectrum.reduce((result, value) => {
                    return Math.max(result, value);
                }, 0).toFixed(3);
                console.info("Voice Input HUD waveform connected: bars", minimum, "..", maximum, "level", voiceLevel.toFixed(3), "pitch", voicePitch.toFixed(3), "timbre", voiceTimbre.toFixed(3), "spectrum max", spectrumMaximum, "flux", voiceSpectralFlux.toFixed(3), "centroid", voiceSpectralCentroid.toFixed(3), "pace", voiceSpeechPace.toFixed(3));
            }
        } catch (error) {
        }
    }

    function connectWaveform() {
        if (waveformSocket) {
            waveformSocket.connected = false;
            waveformSocket.destroy();
        }
        waveformSocket = waveformSocketComponent.createObject(root);
    }

    Component.onCompleted: connectWaveform()

    waveformSocketComponent: Component {
        Socket {
            path: root.waveformPath
            connected: true
            onConnectionStateChanged: {
                if (!connected) {
                    root.resetWaveform();
                    root.waveformReconnectTimer.restart();
                }
            }
            onError: (_error) => {
                root.resetWaveform();
                root.waveformReconnectTimer.restart();
            }

            parser: SplitParser {
                splitMarker: "\n"
                onRead: (data) => {
                    return root.consumeWaveform(data);
                }
            }

        }

    }

    waveformReconnectTimer: Timer {
        interval: 400
        repeat: false
        onTriggered: root.connectWaveform()
    }

    themeFile: FileView {
        path: root.themePath
        watchChanges: true
        blockAllReads: true
        printErrors: false
        onFileChanged: root.refreshTheme()
    }

    themeRefreshTimer: Timer {
        interval: 1000
        repeat: true
        running: true
        triggeredOnStart: true
        onTriggered: root.refreshTheme()
    }

    stateFile: FileView {
        path: root.statePath
        watchChanges: false
        blockAllReads: true
        printErrors: false
    }

    refreshTimer: Timer {
        interval: root.active ? 50 : 100
        repeat: true
        running: true
        triggeredOnStart: true
        onTriggered: root.refreshSnapshot()
    }

}
