import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller

    title: "Hotkey & state"
    description: "Configure the global trigger and state storage."

    SettingsGrid {
        SectionCard {
            theme: root.theme
            title: "Hotkey"

            SettingTextField {
                theme: root.theme
                label: "Accelerator"
                value: root.controller.value("hotkey.accelerator", "")
                help: "Hyprland-style modifier and key description."
                error: root.controller.errorFor("hotkey.accelerator")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("hotkey.accelerator", value);
                }
            }

            SettingCombo {
                theme: root.theme
                label: "Mode"
                value: root.controller.value("hotkey.mode", "toggle")
                labels: ["Hold", "Toggle"]
                values: ["hold", "toggle"]
                error: root.controller.errorFor("hotkey.mode")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("hotkey.mode", value);
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "State"
            showDivider: false

            SettingTextField {
                theme: root.theme
                label: "State file"
                value: root.controller.value("state_file", "auto")
                help: "Use auto, disabled, or an absolute custom path."
                error: root.controller.errorFor("state_file")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("state_file", value);
                }
            }

        }

    }

}
