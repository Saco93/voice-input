import QtQuick
import QtQuick.Controls

Button {
    id: root

    required property QtObject theme
    property bool primary: false
    property bool danger: false

    implicitHeight: 36
    leftPadding: 15
    rightPadding: 15
    font.weight: primary ? Font.DemiBold : Font.Medium
    Accessible.name: text

    contentItem: Text {
        text: root.text
        color: root.primary ? root.theme.background : root.theme.foreground
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    background: Rectangle {
        radius: 8
        color: root.down ? Qt.darker(root.primary ? root.theme.accent : root.theme.elevated, 1.15) : (root.primary ? root.theme.accent : (root.hovered ? Qt.lighter(root.theme.elevated, 1.12) : root.theme.elevated))
        border.width: root.activeFocus ? 2 : 1
        border.color: root.danger ? root.theme.error : (root.activeFocus ? root.theme.accent : root.theme.border)
    }

}
