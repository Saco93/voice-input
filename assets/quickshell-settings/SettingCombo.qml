import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

GridLayout {
    id: root

    required property QtObject theme
    property string label: ""
    property string help: ""
    property string error: ""
    property var labels: []
    property var values: []
    property string value: ""
    property bool enabled: true

    signal selected(string value)

    Layout.fillWidth: true
    columns: width >= 560 ? 2 : 1
    columnSpacing: 24
    rowSpacing: 6

    ColumnLayout {
        Layout.fillWidth: true
        Layout.preferredWidth: 240
        Layout.alignment: Qt.AlignTop
        spacing: 3

        Label {
            text: root.theme.tr(root.label)
            color: root.theme.foreground
            font.pixelSize: 12
            font.weight: Font.Medium
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

        Label {
            visible: root.help.length > 0
            text: root.theme.tr(root.help)
            color: root.theme.subtle
            font.pixelSize: 11
            lineHeight: 1.15
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }

    }

    ColumnLayout {
        Layout.fillWidth: true
        Layout.preferredWidth: 320
        spacing: 4

        ComboBox {
            id: combo

            Layout.fillWidth: true
            model: root.labels
            currentIndex: Math.max(0, root.values.indexOf(root.value))
            enabled: root.enabled
            Accessible.name: root.theme.tr(root.label)
            Accessible.description: root.theme.tr(root.help)
            onActivated: (index) => {
                return root.selected(root.values[index]);
            }

            contentItem: Text {
                leftPadding: 11
                rightPadding: combo.indicator.width + 11
                text: root.theme.tr(combo.currentText)
                color: root.theme.foreground
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }

            background: Rectangle {
                implicitHeight: 34
                radius: 4
                color: root.theme.elevated
                border.width: combo.activeFocus || root.error.length > 0 ? 2 : 1
                border.color: root.error.length > 0 ? root.theme.error : (combo.activeFocus ? root.theme.accent : root.theme.border)
            }

            popup.background: Rectangle {
                color: root.theme.elevated
                border.color: root.theme.border
                radius: 4
            }

            delegate: ItemDelegate {
                required property var modelData
                required property int index

                width: combo.width
                text: root.theme.tr(modelData)
                highlighted: combo.highlightedIndex === index

                contentItem: Text {
                    text: parent.text
                    color: root.theme.foreground
                    verticalAlignment: Text.AlignVCenter
                }

                background: Rectangle {
                    color: parent.highlighted ? root.theme.surface : "transparent"
                }

            }

        }

        Label {
            visible: root.error.length > 0
            text: root.theme.tr(root.error)
            color: root.theme.error
            font.pixelSize: 11
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            Accessible.role: Accessible.AlertMessage
        }

    }

}
