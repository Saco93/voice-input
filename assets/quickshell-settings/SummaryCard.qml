import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property QtObject theme
    property string title: ""
    property string summary: ""
    property string detail: ""
    property bool pending: false

    signal activated()

    Layout.fillWidth: true
    implicitHeight: 116
    radius: 10
    color: root.theme.surface
    border.color: root.theme.border

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 4

        RowLayout {
            Layout.fillWidth: true

            Label {
                text: root.title
                color: root.theme.foreground
                font.pixelSize: 13
                font.weight: Font.DemiBold
                Layout.fillWidth: true
            }

            Label {
                visible: root.pending
                text: "Pending"
                color: root.theme.warning
                font.pixelSize: 10
                font.weight: Font.DemiBold
            }

        }

        Label {
            text: root.summary
            color: root.theme.foreground
            font.pixelSize: 12
            elide: Text.ElideRight
            Layout.fillWidth: true
        }

        Label {
            text: root.detail
            color: root.theme.subtle
            font.pixelSize: 11
            elide: Text.ElideRight
            Layout.fillWidth: true
            Layout.fillHeight: true
        }

        Button {
            text: "Open " + root.title
            flat: true
            leftPadding: 0
            rightPadding: 0
            onClicked: root.activated()

            contentItem: Text {
                text: parent.text
                color: root.theme.accent
                font.pixelSize: 11
                font.weight: Font.Medium
                verticalAlignment: Text.AlignVCenter
            }

        }

    }

}
