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
    border.color: root.theme.border

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 10
        spacing: 8

        RowLayout {
            Layout.fillWidth: true
            Layout.preferredHeight: 46
            spacing: 9

            Rectangle {
                Layout.preferredWidth: 30
                Layout.preferredHeight: 30
                radius: 8
                color: Qt.alpha(root.theme.accent, 0.16)
                border.color: Qt.alpha(root.theme.accent, 0.32)

                Text {
                    anchors.centerIn: parent
                    text: "VI"
                    color: root.theme.accent
                    font.pixelSize: 11
                    font.bold: true
                }

            }

            Label {
                text: root.theme.tr("Voice Input")
                color: root.theme.foreground
                font.pixelSize: 14
                font.weight: Font.DemiBold
                Layout.fillWidth: true
            }

        }

        ListView {
            id: destinationList

            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 3
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
                height: 44
                highlighted: destinationList.currentIndex === index
                leftPadding: 12
                rightPadding: 9
                Accessible.name: root.theme.tr(modelData.title)
                onClicked: root.navigateRequested(modelData.title)

                Rectangle {
                    visible: parent.highlighted
                    anchors.left: parent.left
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    anchors.topMargin: 8
                    anchors.bottomMargin: 8
                    width: 3
                    radius: 2
                    color: root.theme.accent
                }

                contentItem: RowLayout {
                    spacing: 9

                    Text {
                        Layout.preferredWidth: 18
                        text: modelData.glyph
                        color: destinationList.currentIndex === index ? root.theme.accent : root.theme.subtle
                        font.pixelSize: 14
                        horizontalAlignment: Text.AlignHCenter
                    }

                    Text {
                        text: root.theme.tr(modelData.title)
                        color: root.theme.foreground
                        font.pixelSize: 12
                        font.weight: destinationList.currentIndex === index ? Font.DemiBold : Font.Medium
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    Rectangle {
                        visible: errorCount > 0
                        Layout.preferredWidth: Math.max(18, errorNumber.implicitWidth + 8)
                        Layout.preferredHeight: 18
                        radius: 9
                        color: root.theme.error

                        Label {
                            id: errorNumber

                            anchors.centerIn: parent
                            text: String(errorCount)
                            color: root.theme.background
                            font.pixelSize: 9
                            font.bold: true
                        }

                    }

                }

                background: Rectangle {
                    radius: 7
                    color: parent.highlighted ? Qt.alpha(root.theme.accent, 0.1) : (parent.hovered ? root.theme.elevated : "transparent")
                }

            }

        }

        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 72
            radius: 9
            color: Qt.alpha(root.theme.elevated, 0.64)
            border.color: root.theme.border

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 10
                spacing: 4

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 7

                    Rectangle {
                        Layout.preferredWidth: 8
                        Layout.preferredHeight: 8
                        radius: 4
                        color: root.serviceRunning === true ? root.theme.success : (root.serviceRunning === false ? root.theme.error : root.theme.muted)
                    }

                    Label {
                        text: root.theme.tr("Local service")
                        color: root.theme.foreground
                        font.pixelSize: 11
                        font.weight: Font.DemiBold
                        Layout.fillWidth: true
                    }

                }

                Label {
                    text: root.serviceSummary
                    color: root.theme.subtle
                    font.pixelSize: 9
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

            }

        }

    }

}
