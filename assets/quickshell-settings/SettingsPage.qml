import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root

    required property Theme theme
    property string title: ""
    property string description: ""
    default property alias content: body.data

    implicitHeight: pageColumn.implicitHeight + 48

    ColumnLayout {
        id: pageColumn

        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: 24
        spacing: 24

        Label {
            visible: root.description.length > 0
            text: root.theme.tr(root.description)
            color: root.theme.subtle
            font.pixelSize: 12
            lineHeight: 1.25
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

        ColumnLayout {
            id: body

            Layout.fillWidth: true
            spacing: 24
        }

    }

}
