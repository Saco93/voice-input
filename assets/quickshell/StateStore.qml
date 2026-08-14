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
    // Four snapshot fields may each contain a maximum-length transcript, and a
    // JSON control character can require a six-character escape. Leave another
    // MiB for the fixed schema and diagnostics while bounding unknown fields.
    readonly property int maximumStateFileLength: maximumTranscriptLength * 4 * 6 + 1024 * 1024
    readonly property int maximumWaveformLineLength: 16 * 1024
    readonly property var snapshotPhases: ["idle", "arming", "recording", "transcribing", "refining", "outputting", "error"]
    readonly property var hudPositions: ["bottom-center", "bottom-left", "bottom-right"]
    readonly property var emptyWaveform: new Array(waveformBarCount).fill(0)
    readonly property var emptySpectrum: new Array(spectrumBandCount).fill(0)
    property var waveformState: ({
        "bars": emptyWaveform,
        "spectrum": emptySpectrum,
        "spectralFlux": 0,
        "spectralCentroid": 0.5,
        "speechPace": 0,
        "level": 0,
        "pitch": 0.35,
        "timbre": 0.5,
        "frameCount": 0
    })
    readonly property var waveformBars: waveformState.bars
    readonly property var voiceSpectrum: waveformState.spectrum
    readonly property vector4d voiceSpectrum0: Qt.vector4d(voiceSpectrum[0], voiceSpectrum[1], voiceSpectrum[2], voiceSpectrum[3])
    readonly property vector4d voiceSpectrum1: Qt.vector4d(voiceSpectrum[4], voiceSpectrum[5], voiceSpectrum[6], voiceSpectrum[7])
    readonly property vector4d voiceSpectrum2: Qt.vector4d(voiceSpectrum[8], voiceSpectrum[9], voiceSpectrum[10], voiceSpectrum[11])
    readonly property real voiceSpectralFlux: waveformState.spectralFlux
    readonly property real voiceSpectralCentroid: waveformState.spectralCentroid
    readonly property real voiceSpeechPace: waveformState.speechPace
    readonly property real voiceLevel: waveformState.level
    readonly property real voicePitch: waveformState.pitch
    readonly property real voiceTimbre: waveformState.timbre
    readonly property int waveformFrameCount: waveformState.frameCount
    property double waveformSessionId: 0
    property double lastWaveformSequence: -1
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
        "revision": 0,
        "updated_at_ms": 0,
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

    function isPlainObject(value): bool {
        if (value === null || typeof value !== "object" || Array.isArray(value))
            return false;

        const prototype = Object.getPrototypeOf(value);
        return prototype === Object.prototype || prototype === null;
    }

    function snapshotValidationError(candidate) {
        if (!isPlainObject(candidate))
            return "snapshot is not a plain object";

        if (typeof candidate.phase !== "string" || snapshotPhases.indexOf(candidate.phase) < 0)
            return "phase is invalid";

        if (!Number.isSafeInteger(candidate.updated_at_ms) || candidate.updated_at_ms < 0)
            return "updated_at_ms is invalid";

        if (!Number.isSafeInteger(candidate.revision) || candidate.revision < 0)
            return "revision is invalid";

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

    function snapshotForUi(candidate) {
        return {
            "phase": candidate.phase,
            "transcript": candidate.transcript,
            "tooltip": candidate.tooltip,
            "bars": candidate.bars.slice(),
            "hud_enabled": candidate.hud_enabled,
            "hud_margin_bottom": candidate.hud_margin_bottom,
            "hud_height": candidate.hud_height,
            "hud_position": candidate.hud_position,
            "hud_offset_x": candidate.hud_offset_x,
            "hud_offset_y": candidate.hud_offset_y,
            "recording_started_at_ms": candidate.recording_started_at_ms,
            "recording_duration_ms": candidate.recording_duration_ms,
            "revision": candidate.revision,
            "updated_at_ms": candidate.updated_at_ms,
            "error": candidate.error
        };
    }

    function compareSnapshotVersion(candidate): int {
        if (candidate.updated_at_ms < snapshot.updated_at_ms)
            return -1;

        if (candidate.updated_at_ms > snapshot.updated_at_ms)
            return 1;

        if (candidate.revision < snapshot.revision)
            return -1;

        if (candidate.revision > snapshot.revision)
            return 1;

        return 0;
    }

    function reportInvalidSnapshot(reason) {
        invalidSnapshotCount++;
        if (invalidSnapshotCount === 1 || invalidSnapshotCount % 100 === 0)
            console.warn("Voice Input HUD ignored invalid state snapshot:", reason, "count", invalidSnapshotCount);

    }

    function refreshSnapshot() {
        try {
            stateFile.reload();
            const source = stateFile.text();
            if (source.length > maximumStateFileLength) {
                reportInvalidSnapshot("state JSON exceeds the size limit");
                return ;
            }
            const parsed = JSON.parse(source);
            const validationError = snapshotValidationError(parsed);
            if (validationError.length > 0) {
                reportInvalidSnapshot(validationError);
                return ;
            }
            if (compareSnapshotVersion(parsed) <= 0)
                return ;

            const previousPhase = snapshot.phase;
            snapshot = snapshotForUi(parsed);
            invalidSnapshotCount = 0;
            if (parsed.phase !== previousPhase) {
                console.info("Voice Input HUD state:", previousPhase, "->", parsed.phase);
                if (parsed.phase === "idle")
                    resetWaveform();

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
        waveformState = {
            "bars": emptyWaveform.slice(),
            "spectrum": emptySpectrum.slice(),
            "spectralFlux": 0,
            "spectralCentroid": 0.5,
            "speechPace": 0,
            "level": 0,
            "pitch": 0.35,
            "timbre": 0.5,
            "frameCount": 0
        };
    }

    function resetWaveformProtocol() {
        waveformSessionId = 0;
        lastWaveformSequence = -1;
    }

    function isNonnegativeSafeInteger(value): bool {
        return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
    }

    function isUnitNumber(value): bool {
        return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1;
    }

    function isUnitNumberArray(value, expectedLength: int): bool {
        if (!Array.isArray(value) || value.length !== expectedLength)
            return false;

        for (let index = 0; index < value.length; index++) {
            if (!isUnitNumber(value[index]))
                return false;

        }
        return true;
    }

    function waveformValidationError(message): string {
        if (!isPlainObject(message))
            return "message is not a plain object";

        if (message.type !== "reset" && message.type !== "waveform")
            return "type is invalid";

        if (!isNonnegativeSafeInteger(message.session_id) || !isNonnegativeSafeInteger(message.sequence))
            return "protocol identifiers are invalid";

        if (message.type === "reset")
            return "";

        if (message.session_id === 0)
            return "waveform session_id is invalid";

        if (!isUnitNumberArray(message.bars, waveformBarCount))
            return "bars are invalid";

        if (!isUnitNumberArray(message.spectrum, spectrumBandCount))
            return "spectrum is invalid";

        const scalarFields = ["level", "pitch", "timbre", "spectral_flux", "spectral_centroid", "speech_pace"];
        for (let index = 0; index < scalarFields.length; index++) {
            if (!isUnitNumber(message[scalarFields[index]]))
                return scalarFields[index] + " is invalid";

        }
        return "";
    }

    function consumeWaveform(data: string) {
        if (data.length > maximumWaveformLineLength)
            return ;

        try {
            const message = JSON.parse(data);
            if (waveformValidationError(message).length > 0)
                return ;

            if (message.sequence <= lastWaveformSequence)
                return ;

            if (message.type === "reset") {
                lastWaveformSequence = message.sequence;
                waveformSessionId = message.session_id;
                // The daemon closes an accepted recording with session_id 0
                // before the polled snapshot can advance to transcribing. Keep
                // the final live frame through that ordering gap so Listening
                // crossfades directly into processing instead of flashing its
                // silent standby envelope. The idle snapshot clears it shortly
                // afterward; a positive ID still resets a newly starting session.
                if (message.session_id !== 0 || phase === "idle")
                    resetWaveform();

                return ;
            }
            // A client can reconnect in the middle of a session, after its
            // reset frame was already broadcast. Let the first valid waveform
            // establish that connection's session, then reject cross-session
            // frames until the next reset.
            if (waveformSessionId === 0)
                waveformSessionId = message.session_id;
            else if (message.session_id !== waveformSessionId)
                return ;

            const nextState = {
                "bars": message.bars.slice(),
                "spectrum": message.spectrum.slice(),
                "spectralFlux": message.spectral_flux,
                "spectralCentroid": message.spectral_centroid,
                "speechPace": message.speech_pace,
                "level": message.level,
                "pitch": message.pitch,
                "timbre": message.timbre,
                "frameCount": waveformFrameCount + 1
            };
            waveformState = nextState;
            lastWaveformSequence = message.sequence;
            if (nextState.frameCount === 30) {
                const minimum = nextState.bars.reduce((result, value) => {
                    return Math.min(result, value);
                }, 1).toFixed(3);
                const maximum = nextState.bars.reduce((result, value) => {
                    return Math.max(result, value);
                }, 0).toFixed(3);
                const spectrumMaximum = nextState.spectrum.reduce((result, value) => {
                    return Math.max(result, value);
                }, 0).toFixed(3);
                console.info("Voice Input HUD waveform connected: bars", minimum, "..", maximum, "level", nextState.level.toFixed(3), "pitch", nextState.pitch.toFixed(3), "timbre", nextState.timbre.toFixed(3), "spectrum max", spectrumMaximum, "flux", nextState.spectralFlux.toFixed(3), "centroid", nextState.spectralCentroid.toFixed(3), "pace", nextState.speechPace.toFixed(3));
            }
        } catch (error) {
        }
    }

    function connectWaveform() {
        resetWaveformProtocol();
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
