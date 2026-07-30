import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

MenuItem {
    id: root

    required property QtObject theme
    property string leadingText: ""

    implicitWidth: 164
    implicitHeight: 34
    leftPadding: 10
    rightPadding: 10
    topPadding: 0
    bottomPadding: 0

    indicator: Item {
        implicitWidth: 0
        implicitHeight: 0
    }

    contentItem: RowLayout {
        spacing: 8

        Label {
            Layout.preferredWidth: 16
            text: root.leadingText
            color: root.checked ? root.theme.accent : root.theme.subtle
            font.pixelSize: 12
            font.weight: Font.DemiBold
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
        }

        Label {
            Layout.fillWidth: true
            text: root.text
            color: root.enabled ? root.theme.foreground : root.theme.subtle
            font.pixelSize: 11
            font.weight: root.checked ? Font.DemiBold : Font.Normal
            elide: Text.ElideRight
            verticalAlignment: Text.AlignVCenter
        }

    }

    background: Rectangle {
        radius: 4
        color: root.down ? Qt.darker(root.theme.surface, 1.12) : (root.highlighted ? root.theme.surface : "transparent")
        border.width: root.activeFocus ? 1 : 0
        border.color: root.theme.accent
    }

}
