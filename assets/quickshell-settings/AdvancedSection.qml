import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property QtObject theme
    property bool expanded: false
    property string description: "Technical settings for this page."
    default property alias content: body.data

    Layout.fillWidth: true
    implicitHeight: sectionColumn.implicitHeight
    color: "transparent"

    ColumnLayout {
        id: sectionColumn

        anchors.left: parent.left
        anchors.right: parent.right
        spacing: 12

        ItemDelegate {
            Layout.fillWidth: true
            implicitHeight: 48
            leftPadding: 12
            rightPadding: 12
            topPadding: 0
            bottomPadding: 0
            activeFocusOnTab: true
            Accessible.name: root.theme.tr((root.expanded ? "Hide " : "Show ") + "advanced settings")
            onClicked: root.expanded = !root.expanded

            contentItem: RowLayout {
                spacing: 12

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2

                    Label {
                        text: root.theme.tr("Advanced")
                        color: root.theme.foreground
                        font.family: root.theme.fontFamily
                        font.pixelSize: 14
                        font.weight: Font.Black
                    }

                    Label {
                        text: root.theme.tr(root.description)
                        color: root.theme.subtle
                        font.family: root.theme.fontFamily
                        font.weight: Font.ExtraBold
                        font.pixelSize: 11
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }

                }

                Label {
                    text: root.theme.tr(root.expanded ? "Hide" : "Show")
                    color: root.theme.accent
                    font.family: root.theme.fontFamily
                    font.pixelSize: 11
                    font.weight: Font.ExtraBold
                }

            }

            background: Rectangle {
                radius: 4
                color: parent.hovered ? Qt.alpha(root.theme.elevated, 0.55) : "transparent"
                border.width: parent.activeFocus ? 1 : 0
                border.color: root.theme.accent
            }

        }

        ColumnLayout {
            id: body

            visible: root.expanded
            Layout.fillWidth: true
            Layout.topMargin: 4
            spacing: 24
        }

    }

}
