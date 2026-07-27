import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Hyprland
import Quickshell.Wayland

PanelWindow {
    // Procedural slow pulse while the pipeline prepares, so the HUD
    // clearly reads as "not recording yet".

    id: panel

    required property var screenModel
    required property var store
    readonly property bool isFocusedScreen: {
        const focused = Hyprland.focusedMonitor;
        const monitor = Hyprland.monitorFor(screenModel);
        // Monitor wrapper objects are recreated during output hotplug. Compare
        // stable monitor names instead of QObject identity so a reconnected
        // display can become the HUD target immediately.
        return !focused || !monitor || monitor.name === focused.name;
    }
    readonly property string displayText: {
        if (store.transcript.trim().length > 0)
            return store.transcript.trim();

        if (store.phase === "arming")
            return store.tooltip.trim().length > 0 ? store.tooltip.trim() : "Arming microphone…";

        if (store.phase === "recording")
            return "Listening…";

        if (store.phase === "transcribing")
            return "Transcribing…";

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
    // than a bar strip: loudness drives the glow strength, the fundamental
    // frequency estimate drives the breathing rate, and the brightness
    // (timbre) estimate drives the hue shift.
    readonly property bool arming: store.phase === "arming"
    readonly property bool recording: store.phase === "recording"
    // Post-recording stages get a quiet breathing glow so the capsule stays
    // visibly alive while the pipeline finishes.
    readonly property bool processing: store.phase === "transcribing" || store.phase === "refining"
    property real vizClock: 0
    property real breathCycles: 0
    property real levelSmoothed: 0
    property real pitchSmoothed: 0.35
    property real timbreSmoothed: 0.5
    readonly property real breathHz: recording ? 0.8 + pitchSmoothed * 3.2 : 1.1
    readonly property real breathPhase: 0.5 + 0.5 * Math.sin(breathCycles * 2 * Math.PI)
    // Post-recording stages use a heartbeat double pulse (lub-dub) instead of
    // the recording sine breathing, so the phases are distinguishable at a
    // glance. Colors stay fixed per phase via `phaseColor`.
    readonly property real processingPulse: {
        const cycle = breathCycles - Math.floor(breathCycles);
        const lub = Math.exp(-Math.pow((cycle - 0.12) / 0.07, 2));
        const dub = 0.65 * Math.exp(-Math.pow((cycle - 0.42) / 0.07, 2));
        return Math.min(1, lub + dub);
    }
    readonly property real glowStrength: {
        if (arming)
            return 0.12 + 0.4 * (0.5 + 0.5 * Math.sin(vizClock * 3.2));

        if (recording)
            return (0.1 + 0.9 * levelSmoothed) * (0.55 + 0.45 * breathPhase);

        if (processing)
            return 0.22 + 0.28 * processingPulse;

        return 0;
    }
    readonly property color glowColor: {
        const accent = store.themeAccent;
        if (!recording || accent.hslHue < 0)
            return panel.phaseColor;

        // Timbre drives the hue: muffled voiced speech shifts downward,
        // bright fricative-heavy speech shifts upward.
        const shifted = accent.hslHue + (timbreSmoothed - 0.5) * 0.16;
        const hue = ((shifted % 1) + 1) % 1;
        return Qt.hsla(hue, accent.hslSaturation, accent.hslLightness, 1);
    }
    readonly property int maxTranscriptHeight: 88
    readonly property int cardWidth: expanded ? 600 : 300
    // A newly mapped layer surface starts with a temporary 0x0 geometry until
    // Hyprland sends its configure event. Keep the capsule transparent during
    // that frame so its center calculation cannot render it at the left edge.
    readonly property bool geometryReady: width >= cardWidth + 48 && height >= 160
    readonly property color phaseColor: {
        if (store.phase === "arming" || store.phase === "recording")
            return store.themeAccent;

        if (store.phase === "transcribing")
            return store.themeWarning;

        if (store.phase === "refining")
            return store.themeRefining;

        if (store.phase === "error")
            return store.themeError;

        return store.themeForeground;
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
        running: (panel.arming || panel.recording || panel.processing) && panel.visible
        // frameTime is the elapsed time in seconds since the previous frame.
        onTriggered: {
            panel.vizClock = (panel.vizClock + frameTime) % 120;
            // Integrate the breathing phase so pitch-driven frequency changes
            // modulate the rate smoothly instead of jumping the phase.
            panel.breathCycles = (panel.breathCycles + frameTime * panel.breathHz) % 120;
            const levelTarget = panel.recording ? store.voiceLevel : 0;
            const levelConstant = levelTarget > panel.levelSmoothed ? 0.06 : 0.22;
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

    MultiEffect {
        anchors.fill: glowShape
        source: glowShape
        blurEnabled: true
        blur: 1
        blurMax: 56
        autoPaddingEnabled: true
        opacity: panel.glowStrength * capsule.opacity
    }

    Rectangle {
        // Animating x/y would expose the provisional pre-configure coordinates.

        id: capsule

        width: panel.cardWidth
        height: Math.max(Math.max(32, store.hudHeight), transcriptViewport.height + 24)
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
        radius: 18
        color: Qt.rgba(0.067, 0.078, 0.106, 0.92)
        border.width: 1
        border.color: Qt.alpha(panel.glowColor, 0.1 + 0.55 * panel.glowStrength)
        opacity: store.hudEnabled && store.active && panel.geometryReady ? 1 : 0

        Item {
            id: transcriptViewport

            width: capsule.width - 40
            height: Math.min(transcriptLabel.implicitHeight, panel.maxTranscriptHeight)
            anchors.centerIn: parent
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
                // Once the five-line viewport is full, keep the newest text
                // visible by moving the complete transcript upward instead
                // of eliding its tail.
                y: Math.min(0, parent.height - implicitHeight)
                text: panel.displayText
                color: "#eef2ff"
                font.family: "Inter"
                font.pixelSize: 14
                font.weight: Font.DemiBold
                horizontalAlignment: panel.expanded ? Text.AlignLeft : Text.AlignHCenter
                wrapMode: Text.WrapAtWordBoundaryOrAnywhere

                Behavior on y {
                    NumberAnimation {
                        duration: 120
                        easing.type: Easing.OutCubic
                    }

                }

            }

            // The capsule is a pill while this viewport is a rectangle, so the
            // top and bottom text rows would poke past the curved border at
            // the corners. Fade the text into the capsule background near
            // those edges instead of clipping it hard.
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: Math.max(0, Math.min(20, (parent.height - 20) / 2))
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

            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: Math.max(0, Math.min(20, (parent.height - 20) / 2))
                visible: height > 0

                gradient: Gradient {
                    GradientStop {
                        position: 0
                        color: Qt.rgba(0.067, 0.078, 0.106, 0)
                    }

                    GradientStop {
                        position: 1
                        color: Qt.rgba(0.067, 0.078, 0.106, 1)
                    }

                }

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
