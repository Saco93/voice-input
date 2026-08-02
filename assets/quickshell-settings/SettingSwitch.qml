import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

RowLayout {
    id: root

    required property QtObject theme
    property string label: ""
    property string help: ""
    property string error: ""
    property bool checked: false
    property bool enabled: true

    signal toggled(bool checked)

    Layout.fillWidth: true
    spacing: 10

    ColumnLayout {
        Layout.fillWidth: true
        spacing: 2

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
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
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

    Switch {
        id: control

        Layout.preferredWidth: 42
        Layout.preferredHeight: 24
        implicitWidth: 42
        implicitHeight: 24
        leftPadding: 0
        rightPadding: 0
        topPadding: 0
        bottomPadding: 0
        checked: root.checked
        enabled: root.enabled
        Accessible.name: root.theme.tr(root.label)
        Accessible.description: root.theme.tr(root.help)
        onToggled: root.toggled(checked)

        indicator: Rectangle {
            anchors.fill: parent
            radius: height / 2
            color: control.checked ? root.theme.accent : root.theme.elevated
            border.color: root.error.length > 0 ? root.theme.error : (control.checked ? root.theme.accent : root.theme.border)

            Rectangle {
                width: 18
                height: 18
                radius: 9
                y: 3
                x: control.checked ? parent.width - width - 3 : 3
                color: control.checked ? root.theme.background : root.theme.foreground

                Behavior on x {
                    NumberAnimation {
                        duration: 120
                    }

                }

            }

        }

        contentItem: Item {
            implicitWidth: 0
        }

    }

}
