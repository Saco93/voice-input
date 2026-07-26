import QtQuick
import Quickshell
import Quickshell.Hyprland
import Quickshell.Wayland

PanelWindow {
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
    // During the arming window (toggle on -> ASR ready) the waveform strip is
    // driven procedurally so the capsule clearly reads as "preparing" instead
    // of showing live microphone levels, which would imply recording already.
    readonly property bool arming: store.phase === "arming"
    property real armingClock: 0
    readonly property int waveformWidth: 130
    readonly property int waveformBarCount: 30
    readonly property int maxTranscriptHeight: 88
    readonly property int cardWidth: expanded ? 600 : 340
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
        running: panel.arming && panel.visible
        // frameTime is the elapsed time in seconds since the previous frame.
        // Wrapping keeps the float small without disturbing the sine phases.
        onTriggered: panel.armingClock = (panel.armingClock + frameTime) % 120
    }

    Rectangle {
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
        radius: height / 2
        color: Qt.rgba(0.067, 0.078, 0.106, 0.92)
        border.width: 1
        border.color: Qt.rgba(1, 1, 1, 0.1)
        opacity: store.hudEnabled && store.active && panel.geometryReady ? 1 : 0

        Row {
            anchors.fill: parent
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 12

            Item {
                width: panel.waveformWidth
                height: parent.height

                Row {
                    anchors.centerIn: parent
                    spacing: 1

                    Repeater {
                        model: panel.waveformBarCount

                        Rectangle {
                            required property int index
                            // Two detuned traveling sine waves sweep the strip while
                            // arming; recording restores the live microphone level.
                            readonly property real armingWave: 0.5 + 0.5 * Math.sin(panel.armingClock * 5 - index * 0.55)
                            readonly property real armingShimmer: 0.5 + 0.5 * Math.sin(panel.armingClock * 2.4 + index * 0.3)
                            readonly property real level: panel.arming ? 0.16 + 0.24 * armingWave + 0.1 * armingShimmer : Math.max(0, Math.min(1, Number(store.bars[index] || 0)))

                            width: 3
                            height: panel.arming || level > 0.001 ? 3 + level * 31 : 0
                            anchors.verticalCenter: parent.verticalCenter
                            radius: 1.5
                            color: panel.arming ? Qt.alpha(panel.phaseColor, 0.45 + 0.45 * armingWave) : panel.phaseColor

                            Behavior on height {
                                enabled: !panel.arming

                                NumberAnimation {
                                    duration: 16
                                    easing.type: Easing.Linear
                                }

                            }

                            Behavior on color {
                                enabled: !panel.arming

                                ColorAnimation {
                                    duration: 120
                                }

                            }

                        }

                    }

                }

            }

            Item {
                id: transcriptViewport

                width: capsule.width - panel.waveformWidth - 12 - 32
                height: Math.min(transcriptLabel.implicitHeight, panel.maxTranscriptHeight)
                anchors.verticalCenter: parent.verticalCenter
                clip: true

                Text {
                    id: transcriptLabel

                    width: parent.width
                    // Gentle breathing on the status text reinforces that the
                    // pipeline is still preparing rather than listening.
                    opacity: panel.arming ? 0.7 + 0.3 * (0.5 + 0.5 * Math.sin(panel.armingClock * 3.2)) : 1
                    // Once the five-line viewport is full, keep the newest text
                    // visible by moving the complete transcript upward instead
                    // of eliding its tail.
                    y: Math.min(0, parent.height - implicitHeight)
                    text: panel.displayText
                    color: "#eef2ff"
                    font.family: "Inter"
                    font.pixelSize: 14
                    font.weight: Font.DemiBold
                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                    renderType: Text.NativeRendering

                    Behavior on y {
                        NumberAnimation {
                            duration: 120
                            easing.type: Easing.OutCubic
                        }

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
        // Animating x/y would expose the provisional pre-configure coordinates.

    }

    mask: Region {
        intersection: Intersection.Subtract
        x: 0
        y: 0
        width: panel.width
        height: panel.height
    }

}
