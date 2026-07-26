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
    implicitHeight: sectionColumn.implicitHeight + 24
    radius: 10
    color: root.theme.surface
    border.color: root.expanded ? Qt.alpha(root.theme.accent, 0.35) : root.theme.border

    ColumnLayout {
        id: sectionColumn

        anchors.fill: parent
        anchors.margins: 12
        spacing: 9

        ItemDelegate {
            Layout.fillWidth: true
            implicitHeight: 42
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
                    font.pixelSize: 12
                    font.weight: Font.Medium
                }

            }

            background: Rectangle {
                color: parent.hovered ? root.theme.elevated : "transparent"
                radius: 8
            }

        }

        Rectangle {
            visible: root.expanded
            Layout.fillWidth: true
            Layout.preferredHeight: 1
            color: root.theme.border
        }

        ColumnLayout {
            id: body

            visible: root.expanded
            Layout.fillWidth: true
            spacing: 10
        }

    }

}
