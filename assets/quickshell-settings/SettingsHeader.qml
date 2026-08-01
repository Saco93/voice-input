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
    Layout.preferredHeight: 58
    color: root.theme.surface

    Rectangle {
        anchors.bottom: parent.bottom
        width: parent.width
        height: 1
        color: root.theme.border
    }

    RowLayout {
        anchors.fill: parent
        anchors.leftMargin: 24
        anchors.rightMargin: 12
        spacing: 8

        Label {
            text: root.theme.tr(root.title)
            color: root.theme.foreground
            font.family: root.theme.fontFamily
            font.pixelSize: 20
            font.weight: Font.Black
            Layout.fillWidth: true
        }

        Label {
            visible: root.controller.dirty
            text: root.theme.tr("Unsaved")
            color: root.theme.warning
            font.family: root.theme.fontFamily
            font.weight: Font.ExtraBold
            font.pixelSize: 11
        }

        ToolButton {
            id: languageButton

            text: root.theme.i18n.locale === "zh-CN" ? "中文" : "EN"
            Accessible.name: root.theme.tr("Change settings language")
            onClicked: languageMenu.open()

            AppMenu {
                id: languageMenu

                theme: root.theme
                y: languageButton.height + 4

                AppMenuItem {
                    theme: root.theme
                    text: "English"
                    leadingText: checked ? "✓" : ""
                    checkable: true
                    checked: root.theme.i18n.locale === "en"
                    onTriggered: root.theme.i18n.setLocale("en")
                }

                AppMenuItem {
                    theme: root.theme
                    text: "简体中文"
                    leadingText: checked ? "✓" : ""
                    checkable: true
                    checked: root.theme.i18n.locale === "zh-CN"
                    onTriggered: root.theme.i18n.setLocale("zh-CN")
                }

            }

            contentItem: Text {
                text: parent.text
                color: root.theme.subtle
                font.family: root.theme.fontFamily
                font.pixelSize: 11
                font.weight: Font.ExtraBold
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }

            background: Rectangle {
                radius: 4
                color: parent.hovered ? root.theme.elevated : "transparent"
                border.width: parent.activeFocus ? 1 : 0
                border.color: root.theme.accent
            }

        }

        ToolButton {
            id: overflowButton

            text: "⋮"
            Accessible.name: root.theme.tr("More actions")
            onClicked: overflowMenu.open()

            AppMenu {
                id: overflowMenu

                theme: root.theme
                y: overflowButton.height + 4

                AppMenuItem {
                    theme: root.theme
                    text: root.theme.tr(root.controller.loading ? "Reloading…" : "Reload settings")
                    leadingText: "↻"
                    enabled: !root.controller.busy
                    onTriggered: root.controller.reload()
                }

            }

            contentItem: Text {
                text: parent.text
                color: root.theme.subtle
                font.family: root.theme.fontFamily
                font.weight: Font.ExtraBold
                font.pixelSize: 18
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }

            background: Rectangle {
                color: parent.hovered ? root.theme.elevated : "transparent"
                radius: 4
                border.width: parent.activeFocus ? 1 : 0
                border.color: root.theme.accent
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
