import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

GridLayout {
    id: root

    required property QtObject theme
    property string label: ""
    property string value: ""
    property string help: ""
    property string error: ""
    property string placeholderText: ""
    property bool enabled: true
    property int maximumLength: 524288

    signal edited(string value)

    Layout.fillWidth: true
    columns: width >= 560 ? 2 : 1
    columnSpacing: 24
    rowSpacing: 6

    ColumnLayout {
        Layout.fillWidth: true
        Layout.preferredWidth: 240
        Layout.alignment: Qt.AlignTop
        spacing: 3

        Label {
            text: root.theme.tr(root.label)
            color: root.theme.foreground
            font.family: root.theme.fontFamily
            font.pixelSize: 12
            font.weight: Font.ExtraBold
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

        Label {
            visible: root.help.length > 0
            text: root.theme.tr(root.help)
            color: root.theme.subtle
            font.family: root.theme.fontFamily
            font.weight: Font.ExtraBold
            font.pixelSize: 11
            lineHeight: 1.15
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }
    }

    ColumnLayout {
        Layout.fillWidth: true
        Layout.preferredWidth: 320
        spacing: 4

        ScrollView {
            Layout.fillWidth: true
            Layout.preferredHeight: 150
            clip: true

            TextArea {
                id: editor

                text: root.value
                placeholderText: root.theme.tr(root.placeholderText)
                enabled: root.enabled
                selectByMouse: true
                wrapMode: TextEdit.Wrap
                color: root.theme.foreground
                placeholderTextColor: root.theme.subtle
                font.family: root.theme.fontFamily
                font.weight: Font.ExtraBold
                Accessible.name: root.theme.tr(root.label)
                Accessible.description: root.theme.tr(root.help)
                onTextChanged: {
                    if (text.length > root.maximumLength) {
                        remove(root.maximumLength, text.length);
                        return;
                    }
                    if (activeFocus)
                        root.edited(text);
                }

                background: Rectangle {
                    radius: 4
                    color: root.theme.elevated
                    border.width: editor.activeFocus || root.error.length > 0 ? 2 : 1
                    border.color: root.error.length > 0 ? root.theme.error : (editor.activeFocus ? root.theme.accent : root.theme.border)
                }
            }
        }

        Label {
            visible: root.error.length > 0
            text: root.theme.tr(root.error)
            color: root.theme.error
            font.family: root.theme.fontFamily
            font.weight: Font.ExtraBold
            font.pixelSize: 11
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            Accessible.role: Accessible.AlertMessage
        }
    }
}
