import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Hyprland
import Quickshell.Wayland

PanelWindow {
    // Procedural slow pulse while the pipeline prepares, so the HUD
    // clearly reads as "not recording yet".

    id: panel

    required property ShellScreen screenModel
    required property StateStore store
    required property string fontFamily
    readonly property bool isFocusedScreen: {
        const focused = Hyprland.focusedMonitor;
        const monitor = Hyprland.monitorFor(screenModel);
        // Monitor wrapper objects are recreated during output hotplug. Compare
        // stable monitor names instead of QObject identity so a reconnected
        // display can become the HUD target immediately.
        return !focused || !monitor || monitor.name === focused.name;
    }
    readonly property string displayText: {
        if (store.phase === "recording" && (store.tooltip.includes("recording continues") || store.tooltip === "Replaying buffered audio…")) {
            const recoveryNotice = store.tooltip.trim();
            return store.transcript.trim().length > 0 ? store.transcript.trim() + "\n\n" + recoveryNotice : recoveryNotice;
        }
        if (store.transcript.trim().length > 0)
            return store.transcript.trim();

        if (store.phase === "arming")
            return store.tooltip.trim().length > 0 ? store.tooltip.trim() : "Arming microphone…";

        if (store.phase === "recording")
            return "Listening…";

        if (store.phase === "transcribing")
            return "Finalizing transcript…";

        if (store.phase === "refining")
            return "Refining transcript…";

        if (store.phase === "outputting")
            return "Sending text…";

        if (store.phase === "error")
            return store.errorText.length > 0 ? store.errorText : "Voice input error";

        return "";
    }
    readonly property bool expanded: store.transcript.trim().length > 0 || displayText.length > 18 || ["transcribing", "refining", "outputting", "error"].includes(store.phase)
    // Voice visualization is a breathing halo around the whole capsule rather
    // than a bar strip. During Listening, loudness drives reach and strength,
    // pitch drives breathing speed and perimeter wave count, and timbre drives
    // hue plus the wave's harmonic character.
    readonly property bool arming: store.phase === "arming"
    readonly property bool recording: store.phase === "recording"
    readonly property bool finalizing: store.phase === "transcribing"
    readonly property bool refining: store.phase === "refining"
    readonly property bool outputting: store.phase === "outputting"
    // Post-recording stages get a quiet breathing glow so the capsule stays
    // visibly alive while the pipeline finishes and sends the result.
    readonly property bool processing: finalizing || refining || outputting
    readonly property real haloStage: arming ? 0 : recording ? 1 : finalizing ? 2 : refining ? 3 : outputting ? 4 : -1
    property real vizClock: 0
    property real paceClock: 0
    property real waveTravel: 0
    property real breathCycles: 0
    property real levelSmoothed: 0
    property int observedHaloStage: -1
    property color processingColorFrom: store.themeAccent
    property color processingColorTo: store.themeAccent
    property color lastRecordingColor: store.themeAccent
    property real processingColorProgress: 1
    property real processingGeometryBlend: 1
    property real speechPaceSmoothed: 0

    // Initialize transition state synchronously with the phase change. Waiting
    // for the next FrameAnimation tick would expose one frame at progress 1,
    // then jump back to progress 0 before beginning the intended crossfade.
    onHaloStageChanged: {
        const previousStage = observedHaloStage;
        if (processing) {
            if (previousStage >= 2) {
                const easedProgress = processingColorProgress * processingColorProgress * (3 - 2 * processingColorProgress);
                processingColorFrom = mixColors(processingColorFrom, processingColorTo, easedProgress);
            } else {
                processingColorFrom = lastRecordingColor;
            }
            processingColorTo = phaseColor;
            processingColorProgress = 0;
            processingGeometryBlend = previousStage === 1 ? 0 : 1;
        }
        observedHaloStage = haloStage;
    }
    property real transcriptPaceSmoothed: 0
    property real transcriptSessionStart: 0
    property real lastTranscriptGrowthClock: 0
    property int lastTranscriptUnits: 0
    property string lastMeasuredTranscript: ""
    property var transcriptUnitTimes: []
    property int transcriptUnitStartIndex: 0
    property bool paceWasRecording: false
    property real pitchSmoothed: 0.35
    property real timbreSmoothed: 0.5
    // Listening rests at a slow cadence while the user is silent, then blends
    // into pitch-responsive motion as speech grows louder. Every post-recording
    // stage shares one processing cadence so phase changes cannot restart it.
    readonly property real speechBreathHz: 0.48 + pitchSmoothed * 0.35
    // Analyzer levels occupy only part of the normalized range during ordinary
    // speech. Remove the silence floor, then expand that useful range so voice
    // activity produces the full visual response instead of a subtle delta.
    readonly property real voiceActivity: {
        const normalized = Math.max(0, Math.min(1, (levelSmoothed - 0.02) / 0.28));
        return normalized * normalized * (3 - 2 * normalized);
    }
    readonly property real breathHz: recording ? 0.42 + (speechBreathHz - 0.42) * voiceActivity : processing ? 0.48 : 1.1
    // Incremental ASR throughput becomes authoritative as recognized units
    // arrive; acoustic onset density provides immediate fallback at startup.
    readonly property real transcriptPaceConfidence: Math.min(1, lastTranscriptUnits / 6)
    readonly property real effectiveSpeechPace: speechPaceSmoothed * (1 - 0.8 * transcriptPaceConfidence) + transcriptPaceSmoothed * 0.8 * transcriptPaceConfidence
    readonly property real waveTravelSpeed: recording ? 72 + 12 * voiceActivity + 120 * effectiveSpeechPace : 72
    readonly property real breathPhase: 0.5 + 0.5 * Math.sin(breathCycles * 2 * Math.PI)
    // One zero-slope pulse continues across Finalizing, Refining, and Sending.
    readonly property real processingPulse: {
        const cycle = breathCycles - Math.floor(breathCycles);
        return 0.5 - 0.5 * Math.cos(cycle * 2 * Math.PI);
    }
    readonly property real glowStrength: {
        if (arming)
            return 0.12 + 0.4 * (0.5 + 0.5 * Math.sin(vizClock * 3.2));

        if (recording) {
            // A visible ambient breath communicates that Listening is ready,
            // even before speech arrives. Voice energy layers on top without
            // creating a discontinuity when the user starts talking.
            const waitingGlow = 0.32 + 0.18 * breathPhase;
            const voiceGlow = 0.58 * voiceActivity * (0.75 + 0.25 * breathPhase);
            return Math.min(1, waitingGlow + voiceGlow);
        }
        if (processing)
            return 0.42 + 0.32 * processingPulse;

        return 0;
    }
    readonly property color glowColor: {
        const accent = store.themeAccent;
        if (!recording || accent.hslHue < 0)
            return panel.phaseColor;

        // Spectral centroid and high-band energy shift hue with the current
        // phoneme; fast spectral change briefly brightens and saturates it.
        const spectralTone = 0.15 * timbreSmoothed + 0.85 * store.voiceSpectralCentroid;
        const shifted = accent.hslHue + (spectralTone - 0.5) * 0.38 * voiceActivity;
        const hue = ((shifted % 1) + 1) % 1;
        const saturation = Math.min(1, accent.hslSaturation * (0.9 + 0.1 * store.voiceSpectralFlux));
        const lightness = Math.min(1, accent.hslLightness + 0.06 * store.voiceSpectralFlux);
        return Qt.hsla(hue, saturation, lightness, 1);
    }
    // Keep a four-line viewport anchored to the newest text. Only the oldest
    // visible row enters the top fade; the three rows below remain clear.
    readonly property int transcriptViewportHeight: 68
    readonly property int transcriptEdgeFadeHeight: 16
    readonly property int statusBarHeight: 22
    readonly property int cardWidth: expanded ? 600 : 300
    property real clockNowMs: Date.now()
    property real recordingClockAccumulator: 0
    property real statusIndicatorCycles: 0
    readonly property real statusIndicatorBreath: 0.5 - 0.5 * Math.cos(statusIndicatorCycles * 2 * Math.PI)
    readonly property real displayedRecordingDurationMs: {
        if (recording && store.recordingStartedAtMs > 0)
            return Math.max(store.recordingDurationMs, clockNowMs - store.recordingStartedAtMs);

        return store.recordingDurationMs;
    }
    readonly property string phaseLabel: {
        if (arming)
            return "Arming";

        if (recording)
            return "Listening";

        if (finalizing)
            return "Finalizing";

        if (refining)
            return "Refining";

        if (outputting)
            return "Sending";

        if (store.phase === "error")
            return "Error";

        return "Idle";
    }
    // A newly mapped layer surface starts with a temporary 0x0 geometry until
    // Hyprland sends its configure event. Keep the capsule transparent during
    // that frame so its center calculation cannot render it at the left edge.
    readonly property bool geometryReady: width >= cardWidth + 48 && height >= 160
    readonly property color statusIndicatorColor: {
        if (arming)
            return store.themeForeground;

        if (recording)
            return store.themeAccent;

        if (finalizing)
            return "#AAB7BE";

        if (refining)
            return "#E8A9C4";

        if (outputting)
            return "#7BA7D9";

        if (store.phase === "error")
            return store.themeError;

        return store.themeForeground;
    }
    readonly property color phaseColor: {
        if (store.phase === "arming" || store.phase === "recording")
            return store.themeAccent;

        if (processing) {
            const accent = store.themeAccent;
            if (accent.hslHue < 0)
                return accent;

            // Processing geometry stays identical. Nearby hue, saturation, and
            // lightness shifts provide the only visual distinction by phase.
            const hueOffset = finalizing ? -0.035 : refining ? 0.025 : 0.055;
            const hue = ((accent.hslHue + hueOffset) % 1 + 1) % 1;
            const saturationScale = finalizing ? 0.55 : refining ? 1 : 0.75;
            const lightnessOffset = finalizing ? 0.07 : outputting ? 0.04 : 0;
            return Qt.hsla(hue, Math.min(1, accent.hslSaturation * saturationScale), Math.min(1, accent.hslLightness + lightnessOffset), 1);
        }
        if (store.phase === "error")
            return store.themeError;

        return store.themeForeground;
    }

    function mixColors(fromColor, toColor, amount) {
        const t = Math.max(0, Math.min(1, amount));
        return Qt.rgba(fromColor.r + (toColor.r - fromColor.r) * t, fromColor.g + (toColor.g - fromColor.g) * t, fromColor.b + (toColor.b - fromColor.b) * t, fromColor.a + (toColor.a - fromColor.a) * t);
    }

    function formatRecordingDuration(milliseconds) {
        const totalSeconds = Math.max(0, Math.floor(milliseconds / 1000));
        const minutes = Math.floor(totalSeconds / 60);
        const seconds = totalSeconds % 60;
        const minuteText = minutes < 10 ? "0" + minutes : String(minutes);
        const secondText = seconds < 10 ? "0" + seconds : String(seconds);
        return minuteText + ":" + secondText;
    }

    function transcriptUnits(text) {
        const cjk = text.match(/[\u3400-\u9fff]/g);
        const words = text.match(/[A-Za-z0-9]+/g);
        return (cjk ? cjk.length : 0) + (words ? words.length : 0);
    }

    screen: screenModel
    visible: store.hudEnabled && (store.active || fadeOut.running) && isFocusedScreen
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    focusable: false
    WlrLayershell.namespace: "voice-input-hud"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

    anchors {
        top: true
        bottom: true
        left: true
        right: true
    }

    Timer {
        id: fadeOut

        interval: 220
        repeat: false
        running: !store.active
    }

    FrameAnimation {
        running: (panel.arming || panel.recording || panel.processing || store.phase === "error") && panel.visible
        onRunningChanged: {
            if (running && panel.recording)
                panel.clockNowMs = Date.now();

            if (!running) {
                panel.recordingClockAccumulator = 0;
                panel.speechPaceSmoothed = 0;
                panel.transcriptPaceSmoothed = 0;
                panel.levelSmoothed = 0;
                panel.transcriptUnitTimes = [];
                panel.transcriptUnitStartIndex = 0;
                panel.lastTranscriptUnits = 0;
                panel.lastMeasuredTranscript = "";
                panel.paceWasRecording = false;
            }
        }
        // frameTime is the elapsed time in seconds since the previous frame.
        onTriggered: {
            if (panel.recording) {
                panel.lastRecordingColor = panel.glowColor;
                panel.recordingClockAccumulator += frameTime;
                if (panel.recordingClockAccumulator >= 0.1) {
                    panel.clockNowMs = Math.max(Date.now(), panel.clockNowMs + panel.recordingClockAccumulator * 1000);
                    panel.recordingClockAccumulator = 0;
                }
            } else {
                panel.recordingClockAccumulator = 0;
            }

            if (panel.processing) {
                const enteringProcessing = panel.processingGeometryBlend < 1;
                const colorDuration = enteringProcessing ? 0.36 : 0.65;
                panel.processingColorProgress = Math.min(1, panel.processingColorProgress + frameTime / colorDuration);
                panel.processingGeometryBlend = Math.min(1, panel.processingGeometryBlend + frameTime / 0.36);
            }

            panel.vizClock = (panel.vizClock + frameTime) % 3600;
            // The status lamp has its own calm, phase-independent cadence so
            // every active stage reads as alive even when the halo is steady.
            panel.statusIndicatorCycles = (panel.statusIndicatorCycles + frameTime * 0.32) % 120;
            panel.paceClock += frameTime;
            if (panel.recording && !panel.paceWasRecording) {
                panel.transcriptSessionStart = panel.paceClock;
                panel.lastTranscriptGrowthClock = panel.paceClock;
                panel.lastTranscriptUnits = panel.transcriptUnits(store.transcript);
                panel.lastMeasuredTranscript = store.transcript;
                panel.transcriptUnitTimes = [];
                panel.transcriptUnitStartIndex = 0;
                panel.transcriptPaceSmoothed = 0;
            }
            panel.paceWasRecording = panel.recording;
            // Acoustic onset density reacts before the first ASR partial arrives.
            const rawPace = Math.max(0, Math.min(1, (store.voiceSpeechPace - 0.18) / 0.32));
            const paceTarget = panel.recording ? rawPace * rawPace * (3 - 2 * rawPace) : 0;
            const paceConstant = paceTarget > panel.speechPaceSmoothed ? 0.45 : 1.4;
            panel.speechPaceSmoothed += (paceTarget - panel.speechPaceSmoothed) * (1 - Math.exp(-frameTime / paceConstant));
            // Incremental ASR units are distributed across the interval since
            // the previous partial, then counted in a 2.5-second rolling window.
            let transcriptPaceTarget = 0;
            if (panel.recording) {
                // ASR text changes much less often than frames render. Parse
                // transcript units only after a changed partial instead of
                // running two regular expressions and cloning arrays at 60 Hz.
                if (store.transcript !== panel.lastMeasuredTranscript) {
                    const currentUnits = panel.transcriptUnits(store.transcript);
                    let unitTimes = panel.transcriptUnitTimes.slice();
                    if (currentUnits < panel.lastTranscriptUnits) {
                        // A partial-ASR revision must not let restored units count
                        // a second time inside the same rolling pace window.
                        unitTimes = [];
                        panel.transcriptUnitStartIndex = 0;
                        panel.lastTranscriptGrowthClock = panel.paceClock;
                    } else if (currentUnits > panel.lastTranscriptUnits) {
                        const added = Math.min(32, currentUnits - panel.lastTranscriptUnits);
                        const intervalStart = Math.max(panel.lastTranscriptGrowthClock, panel.paceClock - 2.5);
                        const interval = Math.max(0.1, panel.paceClock - intervalStart);
                        for (let index = 0; index < added; index++) unitTimes.push(intervalStart + interval * (index + 1) / added)
                        panel.lastTranscriptGrowthClock = panel.paceClock;
                    }
                    panel.lastTranscriptUnits = currentUnits;
                    panel.lastMeasuredTranscript = store.transcript;
                    panel.transcriptUnitTimes = unitTimes;
                }
                const cutoff = panel.paceClock - 2.5;
                let startIndex = panel.transcriptUnitStartIndex;
                while (startIndex < panel.transcriptUnitTimes.length && panel.transcriptUnitTimes[startIndex] < cutoff)startIndex++
                if (startIndex > 64) {
                    panel.transcriptUnitTimes = panel.transcriptUnitTimes.slice(startIndex);
                    startIndex = 0;
                }
                panel.transcriptUnitStartIndex = startIndex;
                const observedWindow = Math.max(0.75, Math.min(2.5, panel.paceClock - panel.transcriptSessionStart));
                const unitsPerSecond = (panel.transcriptUnitTimes.length - startIndex) / observedWindow;
                const normalizedRate = Math.max(0, Math.min(1, (unitsPerSecond - 1.5) / 8.5));
                transcriptPaceTarget = normalizedRate * normalizedRate * (3 - 2 * normalizedRate);
            }
            const transcriptPaceConstant = transcriptPaceTarget > panel.transcriptPaceSmoothed ? 0.45 : 1.4;
            panel.transcriptPaceSmoothed += (transcriptPaceTarget - panel.transcriptPaceSmoothed) * (1 - Math.exp(-frameTime / transcriptPaceConstant));
            panel.waveTravel = (panel.waveTravel + frameTime * panel.waveTravelSpeed) % 100000;
            // Integrate the breathing phase so pitch-driven frequency changes
            // modulate the rate smoothly instead of jumping the phase.
            panel.breathCycles = (panel.breathCycles + frameTime * panel.breathHz) % 120;
            const levelTarget = panel.recording ? store.voiceLevel : 0;
            const levelConstant = levelTarget > panel.levelSmoothed ? 0.15 : 0.8;
            panel.levelSmoothed += (levelTarget - panel.levelSmoothed) * (1 - Math.exp(-frameTime / levelConstant));
            panel.pitchSmoothed += (store.voicePitch - panel.pitchSmoothed) * (1 - Math.exp(-frameTime / 0.3));
            panel.timbreSmoothed += (store.voiceTimbre - panel.timbreSmoothed) * (1 - Math.exp(-frameTime / 0.3));
        }
    }

    // Soft halo behind the capsule: a hidden capsule-shaped source is blurred
    // by the GPU, so the glow falls off perfectly smoothly with no banding
    // between stacked layers.
    Rectangle {
        id: glowShape

        width: capsule.width
        height: capsule.height
        x: capsule.x
        y: capsule.y
        radius: capsule.radius
        color: panel.glowColor
        visible: false
        layer.enabled: true
    }

    // Errors retain a simple static capsule glow; normal pipeline stages all
    // use variants of the shared top-edge waveform language below.
    MultiEffect {
        anchors.fill: glowShape
        source: glowShape
        blurEnabled: true
        blur: 1
        blurMax: 56
        autoPaddingEnabled: true
        opacity: panel.glowStrength * capsule.opacity
        visible: store.phase === "error"
    }

    // Listening pins the live spectrum to the straight top edge while the
    // remaining perimeter keeps only a restrained ambient breathing halo.
    WavyHalo {
        id: recordingHalo

        x: capsule.x - haloPadding
        y: capsule.y - haloPadding
        capsuleWidth: capsule.width
        capsuleHeight: capsule.height
        cornerRadius: capsule.radius
        level: panel.voiceActivity
        pitch: panel.pitchSmoothed
        timbre: panel.timbreSmoothed
        // Phase shapes only the sub-threshold ambient profile; the visible
        // speech spectrum itself remains fixed to the top edge.
        phase: panel.waveTravel
        strength: panel.glowStrength
        haloColor: panel.glowColor
        spectrum0: store.voiceSpectrum0
        spectrum1: store.voiceSpectrum1
        spectrum2: store.voiceSpectrum2
        spectralFlux: store.voiceSpectralFlux
        spectralCentroid: store.voiceSpectralCentroid
        breath: panel.breathPhase
        stage: panel.haloStage
        previousHaloColor: panel.processing ? panel.processingColorFrom : panel.glowColor
        colorTransition: panel.processing ? panel.processingColorProgress : 1
        processingBlend: panel.processing ? panel.processingGeometryBlend : 1
        visible: false
        layer.enabled: true
    }

    // One renderer carries the complete active pipeline: preparation,
    // frequency-responsive recording, consolidation, refinement, and output.
    // A small isotropic blur keeps every variant optically attached.
    MultiEffect {
        anchors.fill: recordingHalo
        source: recordingHalo
        blurEnabled: true
        blur: 0.55
        blurMax: 6
        autoPaddingEnabled: true
        opacity: capsule.opacity
        visible: panel.arming || panel.recording || panel.processing
    }

    Rectangle {
        // Animating x/y would expose the provisional pre-configure coordinates.

        id: capsule

        width: panel.cardWidth
        height: Math.max(Math.max(32, store.hudHeight), transcriptViewport.height + panel.statusBarHeight + 24)
        x: {
            if (store.hudPosition === "bottom-left")
                return 24 + store.hudOffsetX;

            if (store.hudPosition === "bottom-right")
                return panel.width - width - 24 + store.hudOffsetX;

            return (panel.width - width) / 2 + store.hudOffsetX;
        }
        y: panel.height - height - Math.max(0, store.hudMarginBottom + store.hudOffsetY)
        // A fixed, modest corner radius keeps the rectangular text viewport
        // fully inside the capsule at any height: twelve pixels below the top
        // edge the curved corner is only about one pixel inset, far less than
        // the twenty-pixel text margins. A pill radius (height / 2) let the
        // top and bottom text rows escape past the curved border instead.
        radius: 14
        color: Qt.rgba(0.067, 0.078, 0.106, 0.96)
        border.width: 1
        border.color: Qt.alpha(store.themeForeground, 0.12)
        opacity: store.hudEnabled && store.active && panel.geometryReady ? 1 : 0

        Item {
            id: transcriptViewport

            width: capsule.width - 40
            height: panel.transcriptViewportHeight
            anchors.top: parent.top
            anchors.topMargin: 12
            anchors.horizontalCenter: parent.horizontalCenter
            clip: true

            Text {
                // NativeRendering text can escape the viewport's clip on some
                // render backends, letting long transcripts spill outside the
                // capsule. The default renderer respects clipping.

                id: transcriptLabel

                width: parent.width
                // Gentle breathing on the status text reinforces that the
                // pipeline is still preparing rather than listening.
                opacity: panel.arming ? 0.7 + 0.3 * (0.5 + 0.5 * Math.sin(panel.vizClock * 3.2)) : 1
                // Center short transcripts in the clear middle area. Once the
                // viewport fills, keep the newest text visible by moving the
                // complete transcript upward instead of eliding its tail.
                y: implicitHeight <= parent.height ? (parent.height - implicitHeight) / 2 : parent.height - implicitHeight
                text: panel.displayText
                color: store.themeForeground
                font.family: panel.fontFamily
                font.pixelSize: 14
                font.weight: Font.Bold
                lineHeight: 1.28
                lineHeightMode: Text.ProportionalHeight
                horizontalAlignment: panel.expanded ? Text.AlignLeft : Text.AlignHCenter
                wrapMode: Text.WrapAtWordBoundaryOrAnywhere

                Behavior on y {
                    NumberAnimation {
                        duration: 120
                        easing.type: Easing.OutCubic
                    }

                }

            }

            // Fade only the oldest visible row near the top edge. The newest
            // three rows, including the bottom row, remain fully unobscured.
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: panel.transcriptEdgeFadeHeight
                visible: height > 0

                gradient: Gradient {
                    GradientStop {
                        position: 0
                        color: Qt.rgba(0.067, 0.078, 0.106, 1)
                    }

                    GradientStop {
                        position: 1
                        color: Qt.rgba(0.067, 0.078, 0.106, 0)
                    }

                }

            }

        }

        Item {
            id: statusBar

            height: panel.statusBarHeight
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.leftMargin: 20
            anchors.rightMargin: 20
            anchors.bottomMargin: 6

            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: 1
                color: Qt.alpha(store.themeForeground, 0.1)
            }

            Row {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                anchors.verticalCenterOffset: 1
                spacing: 7

                Item {
                    width: 8
                    height: 8
                    anchors.verticalCenter: parent.verticalCenter

                    Rectangle {
                        anchors.centerIn: parent
                        width: 8
                        height: 8
                        radius: 4
                        color: panel.statusIndicatorColor
                        opacity: 0.06 + 0.16 * panel.statusIndicatorBreath
                        scale: 0.85 + 0.25 * panel.statusIndicatorBreath

                        Behavior on color {
                            ColorAnimation {
                                duration: 260
                                easing.type: Easing.OutCubic
                            }
                        }
                    }

                    Rectangle {
                        anchors.centerIn: parent
                        width: 5
                        height: 5
                        radius: 2.5
                        color: panel.statusIndicatorColor
                        opacity: 0.48 + 0.52 * panel.statusIndicatorBreath
                        scale: 0.88 + 0.12 * panel.statusIndicatorBreath

                        Behavior on color {
                            ColorAnimation {
                                duration: 260
                                easing.type: Easing.OutCubic
                            }
                        }
                    }
                }

                Text {
                    text: panel.phaseLabel
                    color: Qt.alpha(store.themeForeground, 0.68)
                    font.family: panel.fontFamily
                    font.pixelSize: 11
                    font.weight: Font.Bold
                }

            }

            Text {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                anchors.verticalCenterOffset: 1
                text: panel.formatRecordingDuration(panel.displayedRecordingDurationMs)
                color: Qt.alpha(store.themeForeground, 0.62)
                font.family: panel.fontFamily
                font.pixelSize: 11
                font.weight: Font.Bold
            }

        }

        Behavior on opacity {
            NumberAnimation {
                duration: 180
                easing.type: Easing.OutCubic
            }

        }

        Behavior on width {
            NumberAnimation {
                duration: 170
                easing.type: Easing.OutCubic
            }

        }

        Behavior on height {
            NumberAnimation {
                duration: 140
                easing.type: Easing.OutCubic
            }

        }
        // Position follows the configured layer-surface geometry directly.

    }

    mask: Region {
        intersection: Intersection.Subtract
        x: 0
        y: 0
        width: panel.width
        height: panel.height
    }

}
