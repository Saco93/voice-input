import QtQuick
import QtQuick.Controls

Menu {
    id: root

    required property QtObject theme

    implicitWidth: 176
    topPadding: 6
    bottomPadding: 6
    leftPadding: 6
    rightPadding: 6
    spacing: 2
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside

    background: Rectangle {
        radius: 6
        color: root.theme.elevated
        border.width: 1
        border.color: root.theme.border
    }

}
