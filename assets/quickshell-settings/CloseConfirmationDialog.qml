import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Dialog {
    id: root

    required property Theme theme
    required property SettingsController controller

    width: 440
    height: 180
    x: Math.round((parent.width - width) / 2)
    y: Math.round((parent.height - height) / 2)
    modal: true
    title: root.theme.tr("Discard unsaved changes?")
    standardButtons: Dialog.NoButton

    background: Rectangle {
        color: root.theme.surface
        border.color: root.theme.border
        radius: 8
    }

    contentItem: ColumnLayout {
        spacing: 18

        Label {
            text: root.theme.tr("Your configuration or credential replacements have not been saved.")
            color: root.theme.foreground
            wrapMode: Text.WordWrap
            Layout.preferredWidth: 380
        }

        RowLayout {
            Layout.alignment: Qt.AlignRight

            AppButton {
                theme: root.theme
                text: "Keep editing"
                onClicked: root.close()
            }

            AppButton {
                theme: root.theme
                text: "Discard and close"
                danger: true
                onClicked: {
                    root.controller.discard();
                    root.close();
                    Qt.quit();
                }
            }

        }

    }

}
