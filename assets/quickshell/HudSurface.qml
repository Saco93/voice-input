import QtQuick
import Quickshell
import Quickshell.Hyprland
import Quickshell.Wayland

PanelWindow {
    id: panel

    required property var screenModel
    required property var store

    screen: screenModel
    visible: (store.active || fadeOut.running) && isFocusedScreen
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    focusable: false

    anchors {
        top: true
        bottom: true
        left: true
        right: true
    }

    WlrLayershell.namespace: "voice-input-hud"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

    mask: Region {
        intersection: Intersection.Subtract
        x: 0
        y: 0
        width: panel.width
        height: panel.height
    }

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
    readonly property bool expanded: store.transcript.trim().length > 0
        || displayText.length > 18
        || ["transcribing", "refining", "outputting", "error"].includes(store.phase)
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
        if (store.phase === "transcribing") return store.themeWarning;
        if (store.phase === "refining") return store.themeRefining;
        if (store.phase === "error") return store.themeError;
        return store.themeForeground;
    }

    Timer {
        id: fadeOut
        interval: 220
        repeat: false
        running: !store.active
    }

    Rectangle {
        id: capsule
        width: panel.cardWidth
        height: Math.max(56, transcriptViewport.height + 24)
        x: {
            if (store.hudPosition === "bottom-left")
                return 24 + store.hudOffsetX;
            if (store.hudPosition === "bottom-right")
                return panel.width - width - 24 + store.hudOffsetX;
            return (panel.width - width) / 2 + store.hudOffsetX;
        }
        y: panel.height - height - Math.max(0, 72 + store.hudOffsetY)
        radius: height / 2
        color: Qt.rgba(0.067, 0.078, 0.106, 0.92)
        border.width: 1
        border.color: Qt.rgba(1, 1, 1, 0.10)
        opacity: store.active && panel.geometryReady ? 1 : 0

        Behavior on opacity { NumberAnimation { duration: 180; easing.type: Easing.OutCubic } }
        Behavior on width { NumberAnimation { duration: 170; easing.type: Easing.OutCubic } }
        Behavior on height { NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }
        // Position follows the configured layer-surface geometry directly.
        // Animating x/y would expose the provisional pre-configure coordinates.

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
                            readonly property real level: Math.max(0.0, Math.min(1.0,
                                Number(store.bars[index] || 0)))
                            width: 3
                            height: level <= 0.001 ? 0 : 3 + level * 31
                            anchors.verticalCenter: parent.verticalCenter
                            radius: 1.5
                            color: panel.phaseColor
                            Behavior on height {
                                NumberAnimation { duration: 16; easing.type: Easing.Linear }
                            }
                            Behavior on color { ColorAnimation { duration: 120 } }
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
                        NumberAnimation { duration: 120; easing.type: Easing.OutCubic }
                    }
                }
            }
        }
    }
}
