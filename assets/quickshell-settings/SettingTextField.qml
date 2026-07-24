import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ColumnLayout {
    id: root

    required property QtObject theme
    property string label: ""
    property var value: ""
    property string help: ""
    property string error: ""
    property string placeholderText: ""
    property bool password: false
    property bool enabled: true
    signal edited(string value)

    Layout.fillWidth: true
    spacing: 6

    RowLayout {
        Layout.fillWidth: true
        spacing: 16

        ColumnLayout {
            Layout.preferredWidth: 210
            Layout.minimumWidth: 150
            spacing: 3

            Label {
                text: root.label
                color: root.theme.foreground
                font.pixelSize: 14
                font.weight: Font.Medium
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }
            Label {
                visible: root.help.length > 0
                text: root.help
                color: root.theme.subtle
                font.pixelSize: 11
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }
        }

        TextField {
            id: editor
            Layout.fillWidth: true
            Layout.minimumWidth: 220
            text: String(root.value === undefined || root.value === null ? "" : root.value)
            placeholderText: root.placeholderText
            echoMode: root.password ? TextInput.Password : TextInput.Normal
            enabled: root.enabled
            selectByMouse: true
            color: root.theme.foreground
            placeholderTextColor: root.theme.subtle
            Accessible.name: root.label
            Accessible.description: root.help
            onTextEdited: root.edited(text)

            background: Rectangle {
                implicitHeight: 42
                radius: 8
                color: root.theme.elevated
                border.width: editor.activeFocus || root.error.length > 0 ? 2 : 1
                border.color: root.error.length > 0 ? root.theme.error
                    : (editor.activeFocus ? root.theme.accent : root.theme.border)
            }
        }
    }

    Label {
        visible: root.error.length > 0
        text: root.error
        color: root.theme.error
        font.pixelSize: 12
        wrapMode: Text.WordWrap
        Layout.fillWidth: true
        Layout.leftMargin: 226
        Accessible.role: Accessible.AlertMessage
    }
}
