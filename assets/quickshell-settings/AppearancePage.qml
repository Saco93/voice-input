import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller
    property alias advancedExpanded: appearanceAdvanced.expanded

    title: "Appearance"
    description: "Control HUD visibility and placement."

    SettingsGrid {
        SectionCard {
            theme: root.theme
            title: "HUD"

            SettingSwitch {
                theme: root.theme
                label: "Enable HUD"
                checked: root.controller.value("hud.enabled", true)
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("hud.enabled", checked);
                }
            }

            SettingCombo {
                theme: root.theme
                label: "Position"
                value: root.controller.value("hud.position", "bottom-center")
                labels: ["Bottom center", "Bottom left", "Bottom right"]
                values: ["bottom-center", "bottom-left", "bottom-right"]
                error: root.controller.errorFor("hud.position")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("hud.position", value);
                }
            }

        }

    }

    AdvancedSection {
        id: appearanceAdvanced

        theme: root.theme
        description: "HUD geometry, offsets, and keyboard nudge distance."

        SettingsGrid {
            SectionCard {
                theme: root.theme
                title: "Geometry"

                SettingTextField {
                    theme: root.theme
                    label: "Bottom margin"
                    value: root.controller.value("hud.margin_bottom", 72)
                    help: "Pixels."
                    error: root.controller.errorFor("hud.margin_bottom")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("hud.margin_bottom", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Base height"
                    value: root.controller.value("hud.height", 56)
                    help: "Pixels."
                    error: root.controller.errorFor("hud.height")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("hud.height", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Horizontal offset"
                    value: root.controller.value("hud.offset_x", 0)
                    help: "Signed pixels."
                    error: root.controller.errorFor("hud.offset_x")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("hud.offset_x", value);
                    }
                }

            }

            SectionCard {
                theme: root.theme
                title: "Adjustment"

                SettingTextField {
                    theme: root.theme
                    label: "Vertical offset"
                    value: root.controller.value("hud.offset_y", 0)
                    help: "Signed pixels."
                    error: root.controller.errorFor("hud.offset_y")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("hud.offset_y", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Nudge step"
                    value: root.controller.value("hud.nudge_step", 24)
                    help: "Signed pixel adjustment per nudge command."
                    error: root.controller.errorFor("hud.nudge_step")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("hud.nudge_step", value);
                    }
                }

            }

        }

    }

}
