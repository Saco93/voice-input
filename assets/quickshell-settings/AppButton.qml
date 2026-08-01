import QtQuick
import QtQuick.Controls

Button {
    id: root

    required property QtObject theme
    property bool primary: false
    property bool danger: false

    implicitHeight: 34
    leftPadding: 14
    rightPadding: 14
    font.family: root.theme.fontFamily
    font.weight: primary ? Font.Black : Font.ExtraBold
    Accessible.name: root.theme.tr(text)

    contentItem: Text {
        text: root.theme.tr(root.text)
        color: root.primary ? root.theme.background : root.theme.foreground
        font.family: root.theme.fontFamily
        font.weight: Font.ExtraBold
        font.pixelSize: 11
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    background: Rectangle {
        radius: 4
        color: root.down ? Qt.darker(root.primary ? root.theme.accent : root.theme.elevated, 1.15) : (root.primary ? root.theme.accent : (root.hovered ? Qt.lighter(root.theme.elevated, 1.08) : root.theme.elevated))
        border.width: root.activeFocus ? 2 : 1
        border.color: root.danger ? root.theme.error : (root.activeFocus ? root.theme.accent : root.theme.border)
    }

}
