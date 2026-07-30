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

        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 1
            color: root.theme.border
        }

        ItemDelegate {
            Layout.fillWidth: true
            implicitHeight: 48
            leftPadding: 0
            rightPadding: 0
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
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                    }

                    Label {
                        text: root.theme.tr(root.description)
                        color: root.theme.subtle
                        font.pixelSize: 11
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }

                }

                Label {
                    text: root.theme.tr(root.expanded ? "Hide" : "Show")
                    color: root.theme.accent
                    font.pixelSize: 11
                    font.weight: Font.Medium
                }

            }

            background: Rectangle {
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
