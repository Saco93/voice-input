import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property QtObject theme
    property string title: ""
    property string description: ""
    default property alias content: body.data

    Layout.fillWidth: true
    Layout.alignment: Qt.AlignTop
    implicitHeight: sectionColumn.implicitHeight
    color: "transparent"

    ColumnLayout {
        id: sectionColumn

        anchors.left: parent.left
        anchors.right: parent.right
        spacing: 6

        Label {
            text: root.theme.tr(root.title)
            color: root.theme.foreground
            font.pixelSize: 15
            font.weight: Font.DemiBold
            Layout.fillWidth: true
        }

        Label {
            visible: root.description.length > 0
            text: root.theme.tr(root.description)
            color: root.theme.subtle
            font.pixelSize: 11
            lineHeight: 1.2
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

        ColumnLayout {
            id: body

            Layout.topMargin: root.description.length > 0 ? 12 : 10
            Layout.fillWidth: true
            spacing: 16
        }

        Rectangle {
            Layout.topMargin: 8
            Layout.fillWidth: true
            Layout.preferredHeight: 1
            color: root.theme.border
        }

    }

}
