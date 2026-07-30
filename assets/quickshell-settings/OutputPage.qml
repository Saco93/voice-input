import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller
    property alias advancedExpanded: outputAdvanced.expanded

    title: "Output"
    description: "Control text delivery and input-method coordination."

    SettingsGrid {
        SectionCard {
            theme: root.theme
            title: "Delivery"

            SettingCombo {
                theme: root.theme
                label: "Mode"
                value: root.controller.value("output.mode", "type")
                labels: ["Type", "Clipboard only", "Clipboard paste"]
                values: ["type", "clipboard", "paste"]
                error: root.controller.errorFor("output.mode")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("output.mode", value);
                }
            }

            SettingSwitch {
                theme: root.theme
                label: "Fallback to clipboard"
                checked: root.controller.value("output.fallback_to_clipboard", true)
                help: "Copy text when direct delivery fails."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("output.fallback_to_clipboard", checked);
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Input method"

            SettingSwitch {
                theme: root.theme
                label: "Manage Fcitx5"
                checked: root.controller.value("ime.manage_fcitx5", true)
                help: "Coordinate output with the active Fcitx5 input method."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("ime.manage_fcitx5", checked);
                }
            }

            SettingSwitch {
                theme: root.theme
                label: "Force ASCII before output"
                checked: root.controller.value("ime.force_ascii_before_output", true)
                help: "Switch to ASCII mode before inserting recognized text."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("ime.force_ascii_before_output", checked);
                }
            }

        }

    }

    AdvancedSection {
        id: outputAdvanced

        theme: root.theme
        description: "Delivery timing, paste keys, and XWayland behavior."

        SettingsGrid {
            SectionCard {
                theme: root.theme
                title: "Timing and keys"

                SettingTextField {
                    theme: root.theme
                    label: "Type delay"
                    value: root.controller.value("output.type_delay_ms", 0)
                    help: "Milliseconds between generated key events."
                    error: root.controller.errorFor("output.type_delay_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("output.type_delay_ms", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Pre-type delay"
                    value: root.controller.value("output.pre_type_delay_ms", 140)
                    help: "Milliseconds before delivery starts."
                    error: root.controller.errorFor("output.pre_type_delay_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("output.pre_type_delay_ms", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Paste keys"
                    value: root.controller.value("output.paste_keys", "shift+Insert")
                    error: root.controller.errorFor("output.paste_keys")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("output.paste_keys", value);
                    }
                }

            }

            SectionCard {
                theme: root.theme
                title: "XWayland"
                showDivider: false

                SettingSwitch {
                    theme: root.theme
                    label: "Prefer paste for XWayland"
                    checked: root.controller.value("output.prefer_paste_for_xwayland", true)
                    help: "Avoid garbled direct typing in XWayland clients."
                    enabled: !root.controller.busy
                    onToggled: (checked) => {
                        return root.controller.setValue("output.prefer_paste_for_xwayland", checked);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "XWayland paste keys"
                    value: root.controller.value("output.xwayland_paste_keys", "shift+Insert")
                    error: root.controller.errorFor("output.xwayland_paste_keys")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("output.xwayland_paste_keys", value);
                    }
                }

            }

        }

    }

}
