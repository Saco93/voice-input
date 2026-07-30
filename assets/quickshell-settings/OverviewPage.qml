import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller
    required property var serviceRunning
    required property string serviceSummary
    required property string runtimeSummary
    required property string serviceUnit

    signal navigateRequested(string page)

    function providerLabel(value) {
        return root.theme.tr(value === "alibaba-qwen-realtime" ? "Alibaba Qwen realtime" : "Local CLI");
    }

    function outputLabel(value) {
        if (value === "clipboard")
            return root.theme.tr("Clipboard only");

        if (value === "paste")
            return root.theme.tr("Clipboard paste");

        return root.theme.tr("Direct typing");
    }

    title: "Overview"
    description: "See the local service report and review the settings you are preparing to use."

    Rectangle {
        Layout.fillWidth: true
        implicitHeight: 64
        radius: 10
        color: root.theme.surface
        border.color: root.theme.border

        RowLayout {
            anchors.fill: parent
            anchors.margins: 12
            spacing: 10

            Rectangle {
                Layout.preferredWidth: 10
                Layout.preferredHeight: 10
                radius: 5
                color: root.serviceRunning === true ? root.theme.success : (root.serviceRunning === false ? root.theme.error : root.theme.muted)
            }

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2

                Label {
                    text: root.serviceUnit + " — " + root.serviceSummary
                    color: root.theme.foreground
                    font.pixelSize: 12
                    font.weight: Font.DemiBold
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                Label {
                    text: root.runtimeSummary
                    color: root.theme.subtle
                    font.pixelSize: 10
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

            }

            Label {
                text: root.theme.tr(root.controller.runtimeLoading ? "Refreshing…" : "Local report")
                color: root.theme.subtle
                font.pixelSize: 9
            }

        }

    }

    Rectangle {
        Layout.fillWidth: true
        implicitHeight: 66
        radius: 10
        color: Qt.alpha(root.theme.surface, 0.72)
        border.color: root.theme.border

        RowLayout {
            anchors.fill: parent
            anchors.margins: 12
            spacing: 8

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2

                Label {
                    text: root.theme.tr("Speech")
                    color: root.theme.foreground
                    font.pixelSize: 11
                    font.weight: Font.DemiBold
                }

                Label {
                    text: root.providerLabel(root.controller.value("asr.provider", "local-cli"))
                    color: root.theme.subtle
                    font.pixelSize: 9
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

            }

            Label {
                text: "→"
                color: root.theme.accent
                font.pixelSize: 14
            }

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2

                Label {
                    text: root.theme.tr("Refinement")
                    color: root.theme.foreground
                    font.pixelSize: 11
                    font.weight: Font.DemiBold
                }

                Label {
                    text: root.theme.tr(root.controller.value("llm.enabled", false) ? "Enabled" : "Skipped")
                    color: root.theme.subtle
                    font.pixelSize: 9
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

            }

            Label {
                text: "→"
                color: root.theme.accent
                font.pixelSize: 14
            }

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2

                Label {
                    text: root.theme.tr("Output")
                    color: root.theme.foreground
                    font.pixelSize: 11
                    font.weight: Font.DemiBold
                }

                Label {
                    text: root.outputLabel(root.controller.value("output.mode", "type"))
                    color: root.theme.subtle
                    font.pixelSize: 9
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

            }

        }

    }

    SettingsGrid {
        collapseWidth: 460

        SummaryCard {
            theme: root.theme
            title: "Speech"
            pending: root.controller.dirty
            summary: root.providerLabel(root.controller.value("asr.provider", "local-cli"))
            detail: "Language: " + root.controller.value("asr.language", "simplified-chinese")
            onActivated: root.navigateRequested("Speech")
        }

        SummaryCard {
            theme: root.theme
            title: "Refinement"
            pending: root.controller.dirty
            summary: root.controller.value("llm.enabled", false) ? "Enabled" : "Disabled"
            detail: root.controller.value("llm.model", "").length > 0 ? "Model: " + root.controller.value("llm.model", "") : "Model: provider default"
            onActivated: root.navigateRequested("Refinement")
        }

        SummaryCard {
            theme: root.theme
            title: "Output"
            pending: root.controller.dirty
            summary: root.outputLabel(root.controller.value("output.mode", "type"))
            detail: root.controller.value("ime.manage_fcitx5", true) ? "Coordinates with Fcitx5" : "Leaves Fcitx5 unchanged"
            onActivated: root.navigateRequested("Output")
        }

        SummaryCard {
            theme: root.theme
            title: "Appearance"
            pending: root.controller.dirty
            summary: root.controller.value("hud.enabled", true) ? "HUD visible" : "HUD hidden"
            detail: "Position: " + String(root.controller.value("hud.position", "bottom-center")).replace(/-/g, " ")
            onActivated: root.navigateRequested("Appearance")
        }

    }

    Label {
        visible: root.controller.dirty
        text: root.theme.tr("These summaries include unsaved changes. Save to apply them to Voice Input.")
        color: root.theme.warning
        font.pixelSize: 10
        wrapMode: Text.WordWrap
        Layout.fillWidth: true
    }

}
