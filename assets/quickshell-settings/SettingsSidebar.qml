import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root

    required property Theme theme
    required property SettingsController controller
    required property var destinations
    required property var serviceRunning
    required property string serviceSummary
    property alias currentIndex: destinationList.currentIndex

    signal navigateRequested(string page)

    Layout.preferredWidth: 176
    Layout.minimumWidth: 176
    Layout.maximumWidth: 176
    Layout.fillHeight: true
    color: Qt.darker(root.theme.surface, 1.06)

    Rectangle {
        anchors.right: parent.right
        width: 1
        height: parent.height
        color: root.theme.border
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.topMargin: 18
        anchors.bottomMargin: 14
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        spacing: 14

        Label {
            text: root.theme.tr("Voice Input")
            color: root.theme.foreground
            font.family: root.theme.fontFamily
            font.pixelSize: 16
            font.weight: Font.Black
            Layout.fillWidth: true
            Layout.leftMargin: 8
        }

        ListView {
            id: destinationList

            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 2
            clip: true
            model: root.destinations
            currentIndex: 0
            activeFocusOnTab: true
            Accessible.name: root.theme.tr("Settings destinations")
            keyNavigationWraps: true

            delegate: ItemDelegate {
                required property var modelData
                required property int index
                readonly property int errorCount: root.controller.errorCountForPage(modelData.title)

                width: destinationList.width
                height: 38
                highlighted: destinationList.currentIndex === index
                leftPadding: 10
                rightPadding: 8
                Accessible.name: root.theme.tr(modelData.title)
                Accessible.description: errorCount > 0 ? String(errorCount) + " " + root.theme.tr("errors") : ""
                onClicked: root.navigateRequested(modelData.title)

                contentItem: RowLayout {
                    spacing: 8

                    Text {
                        text: root.theme.tr(modelData.title)
                        color: parent.parent.highlighted ? root.theme.foreground : root.theme.subtle
                        font.family: root.theme.fontFamily
                        font.pixelSize: 12
                        font.weight: parent.parent.highlighted ? Font.Black : Font.Bold
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    Label {
                        visible: errorCount > 0
                        text: String(errorCount)
                        color: root.theme.error
                        font.family: root.theme.fontFamily
                        font.pixelSize: 11
                        font.weight: Font.Black
                    }

                }

                background: Rectangle {
                    radius: 4
                    color: parent.highlighted ? root.theme.elevated : (parent.hovered ? Qt.alpha(root.theme.foreground, 0.04) : "transparent")
                    border.width: parent.activeFocus ? 1 : 0
                    border.color: root.theme.accent
                }

            }

        }

        Item {
            Layout.fillWidth: true
            Layout.preferredHeight: 44

            Rectangle {
                anchors.top: parent.top
                width: parent.width
                height: 1
                color: root.theme.border
            }

            ColumnLayout {
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.topMargin: 14
                anchors.leftMargin: 8
                anchors.rightMargin: 8
                spacing: 3

                Label {
                    text: root.theme.tr("Local service")
                    color: root.theme.foreground
                    font.family: root.theme.fontFamily
                    font.pixelSize: 11
                    font.weight: Font.ExtraBold
                    Layout.fillWidth: true
                }

                Label {
                    text: root.serviceSummary
                    color: root.serviceRunning === false ? root.theme.error : root.theme.subtle
                    font.family: root.theme.fontFamily
                    font.weight: Font.ExtraBold
                    font.pixelSize: 11
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

            }

        }

    }

}
