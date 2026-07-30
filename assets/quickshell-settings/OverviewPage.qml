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
    description: "Service status and current configuration."

    SectionCard {
        theme: root.theme
        title: "Local service"

        RowLayout {
            Layout.fillWidth: true
            spacing: 16

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 3

                Label {
                    text: root.serviceUnit
                    color: root.theme.foreground
                    font.pixelSize: 12
                    font.weight: Font.Medium
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                Label {
                    text: root.runtimeSummary
                    color: root.theme.subtle
                    font.pixelSize: 11
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

            }

            Label {
                text: root.serviceSummary
                color: root.serviceRunning === false ? root.theme.error : root.theme.foreground
                font.pixelSize: 12
                font.weight: Font.Medium
            }

        }

    }

    SectionCard {
        theme: root.theme
        title: "Current configuration"
        showDivider: false

        SettingsGrid {
            spacing: 0

            SummaryCard {
                theme: root.theme
                title: "Speech"
                summary: root.providerLabel(root.controller.value("asr.provider", "local-cli"))
                detail: "Language: " + root.controller.value("asr.language", "simplified-chinese")
                onActivated: root.navigateRequested("Speech")
            }

            SummaryCard {
                theme: root.theme
                title: "Refinement"
                summary: root.controller.value("llm.enabled", false) ? "Enabled" : "Disabled"
                detail: root.controller.value("llm.model", "").length > 0 ? "Model: " + root.controller.value("llm.model", "") : "Model: provider default"
                onActivated: root.navigateRequested("Refinement")
            }

            SummaryCard {
                theme: root.theme
                title: "Output"
                summary: root.outputLabel(root.controller.value("output.mode", "type"))
                detail: root.controller.value("ime.manage_fcitx5", true) ? "Coordinates with Fcitx5" : "Leaves Fcitx5 unchanged"
                onActivated: root.navigateRequested("Output")
            }

            SummaryCard {
                theme: root.theme
                title: "Appearance"
                showDivider: false
                summary: root.controller.value("hud.enabled", true) ? "HUD visible" : "HUD hidden"
                detail: "Position: " + String(root.controller.value("hud.position", "bottom-center")).replace(/-/g, " ")
                onActivated: root.navigateRequested("Appearance")
            }

        }

    }

}
