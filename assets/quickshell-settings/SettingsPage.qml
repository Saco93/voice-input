import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root

    required property QtObject theme
    property string title: ""
    property string description: ""
    default property alias content: body.data

    implicitHeight: pageColumn.implicitHeight + 32

    ColumnLayout {
        id: pageColumn

        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: 16
        spacing: 10

        Label {
            visible: root.description.length > 0
            text: root.theme.tr(root.description)
            color: root.theme.subtle
            font.pixelSize: 11
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

        ColumnLayout {
            id: body

            Layout.fillWidth: true
            spacing: 10
        }

    }

}
