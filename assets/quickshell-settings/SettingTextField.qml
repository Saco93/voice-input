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
    spacing: 4

    Label {
        text: root.theme.tr(root.label)
        color: root.theme.foreground
        font.pixelSize: 12
        font.weight: Font.Medium
        wrapMode: Text.WordWrap
        Layout.fillWidth: true
    }

    Label {
        visible: root.help.length > 0
        text: root.theme.tr(root.help)
        color: root.theme.subtle
        font.pixelSize: 10
        wrapMode: Text.WordWrap
        Layout.fillWidth: true
    }

    TextField {
        id: editor

        Layout.fillWidth: true
        text: String(root.value === undefined || root.value === null ? "" : root.value)
        placeholderText: root.theme.tr(root.placeholderText)
        echoMode: root.password ? TextInput.Password : TextInput.Normal
        enabled: root.enabled
        selectByMouse: true
        color: root.theme.foreground
        placeholderTextColor: root.theme.subtle
        Accessible.name: root.theme.tr(root.label)
        Accessible.description: root.theme.tr(root.help)
        onTextEdited: root.edited(text)

        background: Rectangle {
            implicitHeight: 36
            radius: 7
            color: root.theme.elevated
            border.width: editor.activeFocus || root.error.length > 0 ? 2 : 1
            border.color: root.error.length > 0 ? root.theme.error : (editor.activeFocus ? root.theme.accent : root.theme.border)
        }

    }

    Label {
        visible: root.error.length > 0
        text: root.theme.tr(root.error)
        color: root.theme.error
        font.pixelSize: 11
        wrapMode: Text.WordWrap
        Layout.fillWidth: true
        Accessible.role: Accessible.AlertMessage
    }

}
