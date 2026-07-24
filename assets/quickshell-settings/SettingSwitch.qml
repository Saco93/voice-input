import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

RowLayout {
    id: root

    required property QtObject theme
    property string label: ""
    property string help: ""
    property bool checked: false
    property bool enabled: true
    signal toggled(bool checked)

    Layout.fillWidth: true
    spacing: 16

    ColumnLayout {
        Layout.fillWidth: true
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

    Switch {
        id: control
        checked: root.checked
        enabled: root.enabled
        Accessible.name: root.label
        Accessible.description: root.help
        onToggled: root.toggled(checked)

        indicator: Rectangle {
            implicitWidth: 46
            implicitHeight: 26
            x: control.leftPadding
            y: parent.height / 2 - height / 2
            radius: height / 2
            color: control.checked ? root.theme.accent : root.theme.elevated
            border.color: control.checked ? root.theme.accent : root.theme.border

            Rectangle {
                width: 20
                height: 20
                radius: 10
                y: 3
                x: control.checked ? parent.width - width - 3 : 3
                color: control.checked ? root.theme.background : root.theme.foreground
                Behavior on x { NumberAnimation { duration: 120 } }
            }
        }
        contentItem: Item { implicitWidth: 0 }
    }
}
