import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property QtObject theme
    property string title: ""
    property string summary: ""
    property string detail: ""
    property bool showDivider: true

    signal activated()

    Layout.fillWidth: true
    implicitHeight: 58
    color: "transparent"

    RowLayout {
        anchors.fill: parent
        spacing: 16

        Label {
            text: root.theme.tr(root.title)
            color: root.theme.foreground
            font.pixelSize: 12
            font.weight: Font.Medium
            Layout.preferredWidth: 110
        }

        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2

            Label {
                text: root.theme.tr(root.summary)
                color: root.theme.foreground
                font.pixelSize: 12
                elide: Text.ElideRight
                Layout.fillWidth: true
            }

            Label {
                text: root.theme.tr(root.detail)
                color: root.theme.subtle
                font.pixelSize: 11
                elide: Text.ElideRight
                Layout.fillWidth: true
            }

        }

        Button {
            text: root.theme.tr("Open " + root.title)
            flat: true
            Accessible.name: text
            onClicked: root.activated()

            contentItem: Text {
                text: parent.text
                color: root.theme.accent
                font.pixelSize: 11
                font.weight: Font.Medium
                verticalAlignment: Text.AlignVCenter
            }

            background: Rectangle {
                color: parent.hovered ? Qt.alpha(root.theme.elevated, 0.55) : "transparent"
                border.width: parent.activeFocus ? 1 : 0
                border.color: root.theme.accent
            }

        }

    }

    Rectangle {
        visible: root.showDivider
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: 1
        color: root.theme.border
    }

}
