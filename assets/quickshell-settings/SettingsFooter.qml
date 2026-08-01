import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property Theme theme
    required property SettingsController controller

    Layout.fillWidth: true
    Layout.minimumHeight: 58
    Layout.preferredHeight: 58
    Layout.maximumHeight: 58
    implicitHeight: 58
    color: root.theme.surface
    border.color: root.theme.border

    RowLayout {
        id: footerLayout

        anchors.fill: parent
        anchors.margins: 9
        spacing: 8

        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2

            Label {
                visible: root.controller.globalError.length > 0
                text: root.theme.tr(root.controller.globalError)
                color: root.theme.error
                font.family: root.theme.fontFamily
                font.pixelSize: 11
                font.weight: Font.ExtraBold
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.AlertMessage
            }

            Label {
                visible: root.controller.globalError.length === 0 && root.controller.statusMessage.length > 0
                text: root.theme.tr(root.controller.statusMessage)
                color: root.theme.success
                font.family: root.theme.fontFamily
                font.weight: Font.ExtraBold
                font.pixelSize: 11
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            Label {
                visible: root.controller.globalError.length === 0 && root.controller.statusMessage.length === 0
                text: root.theme.tr(root.controller.saving ? "Saving configuration and restarting service…" : (root.controller.loading ? "Reloading configuration…" : (root.controller.testing ? "Testing LLM settings…" : (root.controller.dirty ? "Changes have not been saved." : "Configuration is up to date."))))
                color: root.controller.dirty ? root.theme.warning : root.theme.subtle
                font.family: root.theme.fontFamily
                font.weight: Font.ExtraBold
                font.pixelSize: 11
            }

        }

        RowLayout {
            Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
            Layout.fillWidth: false
            spacing: 8

            AppButton {
                theme: root.theme
                text: "Discard"
                Layout.fillWidth: false
                enabled: root.controller.dirty && !root.controller.busy
                onClicked: root.controller.discard()
            }

            AppButton {
                theme: root.theme
                text: root.controller.saving ? "Saving…" : "Save & restart"
                primary: true
                Layout.fillWidth: false
                enabled: root.controller.dirty && !root.controller.busy
                onClicked: root.controller.save()
            }

        }

    }

}
