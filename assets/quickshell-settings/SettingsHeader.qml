import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property Theme theme
    required property SettingsController controller
    required property string title

    signal closeRequested()

    Layout.fillWidth: true
    Layout.preferredHeight: 54
    color: root.theme.surface
    border.color: root.theme.border

    RowLayout {
        anchors.fill: parent
        anchors.leftMargin: 16
        anchors.rightMargin: 10
        spacing: 8

        Label {
            text: root.theme.tr(root.title)
            color: root.theme.foreground
            font.pixelSize: 16
            font.weight: Font.DemiBold
            Layout.fillWidth: true
        }

        Label {
            visible: root.controller.dirty
            text: root.theme.tr("Unsaved")
            color: root.theme.warning
            font.pixelSize: 10
        }

        Rectangle {
            Layout.preferredWidth: 88
            Layout.preferredHeight: 30
            radius: 7
            color: root.theme.elevated
            border.color: root.theme.border

            RowLayout {
                anchors.fill: parent
                anchors.margins: 2
                spacing: 2

                Button {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    text: "EN"
                    flat: true
                    Accessible.name: root.theme.tr("Switch settings language to English")
                    onClicked: root.theme.i18n.setLocale("en")

                    contentItem: Text {
                        text: parent.text
                        color: root.theme.i18n.locale === "en" ? root.theme.background : root.theme.subtle
                        font.pixelSize: 10
                        font.weight: Font.DemiBold
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    background: Rectangle {
                        radius: 5
                        color: root.theme.i18n.locale === "en" ? root.theme.accent : (parent.hovered ? Qt.alpha(root.theme.accent, 0.12) : "transparent")
                        border.width: parent.activeFocus ? 1 : 0
                        border.color: root.theme.accent
                    }

                }

                Label {
                    text: "/"
                    color: root.theme.subtle
                    font.pixelSize: 9
                }

                Button {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    text: "中文"
                    flat: true
                    Accessible.name: root.theme.tr("Switch settings language to Simplified Chinese")
                    onClicked: root.theme.i18n.setLocale("zh-CN")

                    contentItem: Text {
                        text: parent.text
                        color: root.theme.i18n.locale === "zh-CN" ? root.theme.background : root.theme.subtle
                        font.pixelSize: 10
                        font.weight: Font.DemiBold
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    background: Rectangle {
                        radius: 5
                        color: root.theme.i18n.locale === "zh-CN" ? root.theme.accent : (parent.hovered ? Qt.alpha(root.theme.accent, 0.12) : "transparent")
                        border.width: parent.activeFocus ? 1 : 0
                        border.color: root.theme.accent
                    }

                }

            }

        }

        ToolButton {
            id: overflowButton

            text: "⋮"
            Accessible.name: root.theme.tr("More actions")
            onClicked: overflowMenu.open()

            Menu {
                id: overflowMenu

                y: overflowButton.height

                MenuItem {
                    text: root.theme.tr(root.controller.loading ? "Reloading…" : "Reload settings")
                    enabled: !root.controller.busy
                    onTriggered: root.controller.reload()
                }

            }

            contentItem: Text {
                text: parent.text
                color: root.theme.foreground
                font.pixelSize: 19
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }

            background: Rectangle {
                color: parent.hovered ? root.theme.elevated : "transparent"
                radius: 7
            }

        }

        AppButton {
            theme: root.theme
            text: "Close"
            enabled: !root.controller.busy
            onClicked: root.closeRequested()
        }

    }

}
