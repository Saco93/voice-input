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
    implicitHeight: cardColumn.implicitHeight + 24
    radius: 10
    color: theme.surface
    border.color: theme.border
    border.width: 1

    ColumnLayout {
        id: cardColumn

        anchors.fill: parent
        anchors.margins: 12
        spacing: 8

        Label {
            text: root.theme.tr(root.title)
            color: root.theme.foreground
            font.pixelSize: 14
            font.weight: Font.DemiBold
            Layout.fillWidth: true
        }

        Label {
            visible: root.description.length > 0
            text: root.theme.tr(root.description)
            color: root.theme.subtle
            font.pixelSize: 11
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
            spacing: 8
        }

    }

}
