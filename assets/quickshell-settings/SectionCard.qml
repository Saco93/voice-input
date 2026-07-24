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
    implicitHeight: cardColumn.implicitHeight + 40
    radius: 14
    color: theme.surface
    border.color: theme.border
    border.width: 1

    ColumnLayout {
        id: cardColumn
        anchors.fill: parent
        anchors.margins: 20
        spacing: 15

        Label {
            text: root.title
            color: root.theme.foreground
            font.pixelSize: 18
            font.weight: Font.DemiBold
            Layout.fillWidth: true
        }

        Label {
            visible: root.description.length > 0
            text: root.description
            color: root.theme.subtle
            font.pixelSize: 13
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

        Rectangle {
            visible: root.description.length > 0
            Layout.fillWidth: true
            Layout.preferredHeight: 1
            color: root.theme.border
        }

        ColumnLayout {
            id: body
            Layout.fillWidth: true
            spacing: 13
        }
    }
}
