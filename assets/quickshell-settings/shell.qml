import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io

ShellRoot {
    id: shell

    Theme { id: theme }
    SettingsController { id: controller }

    IpcHandler {
        target: "voiceInputSettings"
        function activate() { window.visible = true; window.raise(); window.requestActivate(); }
    }

    readonly property var categories: [
        {"title": "General", "caption": "Files and hotkey"},
        {"title": "Audio", "caption": "Capture and pre-roll"},
        {"title": "Recognition", "caption": "Provider and local ASR"},
        {"title": "Alibaba", "caption": "Realtime and final pass"},
        {"title": "Output & IME", "caption": "Delivery and input method"},
        {"title": "LLM", "caption": "Transcript refinement"},
        {"title": "HUD", "caption": "Overlay placement"}
    ]

    FloatingWindow {
        id: window
        title: controller.dirty ? "Voice Input Settings •" : "Voice Input Settings"
        visible: true
        implicitWidth: 1000
        implicitHeight: 760
        minimumSize: Qt.size(760, 600)
        color: theme.background
        onClosed: Qt.quit()

        // FloatingWindow wraps a QQuickWindow. These small forwarding methods
        // expose the standard Qt window APIs used by the single-instance IPC
        // activator while keeping the handler contract stable.
        function raise() {
            if (contentItem.window)
                contentItem.window.raise();
        }
        function requestActivate() {
            if (contentItem.window)
                contentItem.window.requestActivate();
        }
        function requestClose() {
            if (controller.dirty)
                closeDialog.open();
            else
                Qt.quit();
        }

        Rectangle {
            anchors.fill: parent
            color: theme.background

            ColumnLayout {
                anchors.fill: parent
                spacing: 0

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 78
                    color: theme.surface
                    border.color: theme.border

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 24
                        anchors.rightMargin: 18
                        spacing: 14

                        Rectangle {
                            Layout.preferredWidth: 42
                            Layout.preferredHeight: 42
                            radius: 12
                            color: Qt.alpha(theme.accent, 0.18)
                            border.color: Qt.alpha(theme.accent, 0.42)
                            Text {
                                anchors.centerIn: parent
                                text: "VI"
                                color: theme.accent
                                font.pixelSize: 15
                                font.bold: true
                            }
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label {
                                text: "Voice Input Settings"
                                color: theme.foreground
                                font.pixelSize: 22
                                font.weight: Font.DemiBold
                            }
                            Label {
                                text: "Configure capture, recognition, output, and refinement"
                                color: theme.subtle
                                font.pixelSize: 12
                            }
                        }

                        Rectangle {
                            visible: controller.dirty
                            Layout.preferredWidth: dirtyLabel.implicitWidth + 20
                            Layout.preferredHeight: 30
                            radius: 15
                            color: Qt.alpha(theme.warning, 0.14)
                            border.color: Qt.alpha(theme.warning, 0.42)
                            Label {
                                id: dirtyLabel
                                anchors.centerIn: parent
                                text: "Unsaved changes"
                                color: theme.warning
                                font.pixelSize: 12
                                font.weight: Font.Medium
                            }
                        }

                        AppButton {
                            theme: theme
                            text: "Close"
                            onClicked: window.requestClose()
                        }
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    spacing: 0

                    Rectangle {
                        Layout.preferredWidth: window.width < 860 ? 184 : 224
                        Layout.fillHeight: true
                        color: Qt.darker(theme.surface, 1.06)
                        border.color: theme.border

                        ListView {
                            id: categoryList
                            anchors.fill: parent
                            anchors.margins: 14
                            spacing: 6
                            clip: true
                            model: shell.categories
                            currentIndex: 0
                            activeFocusOnTab: true
                            Accessible.name: "Settings categories"
                            keyNavigationWraps: true

                            delegate: ItemDelegate {
                                required property var modelData
                                required property int index
                                width: categoryList.width
                                height: 62
                                highlighted: categoryList.currentIndex === index
                                Accessible.name: modelData.title + ", " + modelData.caption
                                onClicked: {
                                    categoryList.currentIndex = index;
                                    contentScroll.contentItem.contentY = 0;
                                }

                                contentItem: Column {
                                    leftPadding: 8
                                    spacing: 3
                                    Text {
                                        text: modelData.title
                                        color: parent.parent.highlighted ? theme.accent : theme.foreground
                                        font.pixelSize: 14
                                        font.weight: Font.DemiBold
                                    }
                                    Text {
                                        text: modelData.caption
                                        color: theme.subtle
                                        font.pixelSize: 11
                                    }
                                }
                                background: Rectangle {
                                    radius: 9
                                    color: parent.highlighted ? Qt.alpha(theme.accent, 0.14)
                                        : (parent.hovered ? theme.elevated : "transparent")
                                    border.color: parent.highlighted ? Qt.alpha(theme.accent, 0.35)
                                        : "transparent"
                                }
                            }
                        }
                    }

                    ScrollView {
                        id: contentScroll
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        contentWidth: availableWidth
                        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

                        StackLayout {
                            width: contentScroll.availableWidth
                            currentIndex: categoryList.currentIndex

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "General"
                                    description: "Core state and trigger behavior. Enum values are sent using the backend's kebab-case wire format."
                                    SettingTextField {
                                        theme: theme; label: "State file"; value: controller.value("state_file", "auto")
                                        help: "Use auto, disabled, or an absolute custom path."
                                        error: controller.errorFor("state_file"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("state_file", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Hotkey accelerator"; value: controller.value("hotkey.accelerator", "")
                                        help: "Hyprland-style modifier and key description."
                                        error: controller.errorFor("hotkey.accelerator"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("hotkey.accelerator", value)
                                    }
                                    SettingCombo {
                                        theme: theme; label: "Hotkey mode"; value: controller.value("hotkey.mode", "hold")
                                        labels: ["Hold", "Toggle"]; values: ["hold", "toggle"]
                                        error: controller.errorFor("hotkey.mode"); enabled: !controller.busy
                                        onSelected: value => controller.setValue("hotkey.mode", value)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "Audio capture"
                                    description: "Configure the recording source, cadence, and optional always-warm pre-roll buffer."
                                    SettingTextField {
                                        theme: theme; label: "Device"; value: controller.value("audio.device", "default")
                                        error: controller.errorFor("audio.device"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("audio.device", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Sample rate"; value: controller.value("audio.sample_rate", 16000)
                                        help: "Samples per second (Hz)."; error: controller.errorFor("audio.sample_rate"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("audio.sample_rate", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Maximum duration"; value: controller.value("audio.max_duration_secs", 90)
                                        help: "Seconds."; error: controller.errorFor("audio.max_duration_secs"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("audio.max_duration_secs", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Partial interval"; value: controller.value("audio.partial_interval_ms", 1500)
                                        help: "Milliseconds between partial updates."; error: controller.errorFor("audio.partial_interval_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("audio.partial_interval_ms", value)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Enable pre-roll"; checked: controller.value("audio.pre_roll_enabled", false)
                                        help: "Keeps microphone capture warm so the first syllable is preserved."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("audio.pre_roll_enabled", checked)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Pre-roll window"; value: controller.value("audio.pre_roll_ms", 500)
                                        help: "Milliseconds retained before activation."; error: controller.errorFor("audio.pre_roll_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("audio.pre_roll_ms", value)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "Speech recognition"
                                    description: "Choose local CLI recognition or Alibaba realtime ASR, with optional local fallback."
                                    SettingCombo {
                                        theme: theme; label: "Provider"; value: controller.value("asr.provider", "local-cli")
                                        labels: ["Local CLI", "Alibaba Qwen realtime"]
                                        values: ["local-cli", "alibaba-qwen-realtime"]
                                        error: controller.errorFor("asr.provider"); enabled: !controller.busy
                                        onSelected: value => controller.setValue("asr.provider", value)
                                    }
                                    SettingCombo {
                                        theme: theme; label: "Language"; value: controller.value("asr.language", "simplified-chinese")
                                        labels: ["English", "Simplified Chinese", "Traditional Chinese", "Japanese", "Korean"]
                                        values: ["english", "simplified-chinese", "traditional-chinese", "japanese", "korean"]
                                        error: controller.errorFor("asr.language"); enabled: !controller.busy
                                        onSelected: value => controller.setValue("asr.language", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Backend command"; value: controller.value("asr.backend_command", "")
                                        help: "Executable used by the local CLI provider."; error: controller.errorFor("asr.backend_command"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.backend_command", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Local engine"; value: controller.value("asr.engine", "")
                                        error: controller.errorFor("asr.engine"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.engine", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Local model"; value: controller.value("asr.model", "")
                                        placeholderText: "Backend default"; error: controller.errorFor("asr.model"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.model", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Connect timeout"; value: controller.value("asr.connect_timeout_ms", 5000)
                                        help: "Milliseconds."; error: controller.errorFor("asr.connect_timeout_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.connect_timeout_ms", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Finalize timeout"; value: controller.value("asr.finalize_timeout_ms", 8000)
                                        help: "Milliseconds."; error: controller.errorFor("asr.finalize_timeout_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.finalize_timeout_ms", value)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Fallback to local"; checked: controller.value("asr.fallback_to_local", true)
                                        help: "Use the local CLI backend when remote recognition fails."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("asr.fallback_to_local", checked)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "Alibaba realtime"
                                    description: "The API key is managed as an encrypted credential. Leave its replacement field blank to keep the current credential."
                                    SettingTextField {
                                        theme: theme; label: "Endpoint"; value: controller.value("asr.alibaba.endpoint", "")
                                        error: controller.errorFor("asr.alibaba.endpoint"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.endpoint", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Replace Alibaba API key"; value: controller.alibabaCredential
                                        help: controller.credentialLabel("alibaba-api-key") + ". Blank keeps it unchanged."
                                        password: true; placeholderText: "Enter a new credential"
                                        error: controller.errorFor("credentials.alibaba-api-key"); enabled: !controller.busy
                                        onEdited: value => controller.alibabaCredential = value
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Realtime model"; value: controller.value("asr.alibaba.model", "")
                                        error: controller.errorFor("asr.alibaba.model"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.model", value)
                                    }
                                    SettingCombo {
                                        theme: theme; label: "Turn mode"; value: controller.value("asr.alibaba.turn_mode", "server-vad")
                                        labels: ["Server VAD", "Manual commit"]; values: ["server-vad", "manual"]
                                        error: controller.errorFor("asr.alibaba.turn_mode"); enabled: !controller.busy
                                        onSelected: value => controller.setValue("asr.alibaba.turn_mode", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "VAD threshold"; value: controller.value("asr.alibaba.vad_threshold", 0.2)
                                        error: controller.errorFor("asr.alibaba.vad_threshold"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.vad_threshold", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Silence duration"; value: controller.value("asr.alibaba.silence_duration_ms", 400)
                                        help: "Milliseconds."; error: controller.errorFor("asr.alibaba.silence_duration_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.silence_duration_ms", value)
                                    }
                                }
                                SectionCard {
                                    theme: theme
                                    title: "Alibaba final pass"
                                    description: "Optionally retranscribe the complete audio after the realtime turn finishes."
                                    SettingSwitch {
                                        theme: theme; label: "Enable final pass"; checked: controller.value("asr.alibaba.final_pass_enabled", false)
                                        enabled: !controller.busy
                                        onToggled: checked => controller.setValue("asr.alibaba.final_pass_enabled", checked)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Base URL"; value: controller.value("asr.alibaba.final_pass_base_url", "")
                                        error: controller.errorFor("asr.alibaba.final_pass_base_url"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.final_pass_base_url", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Model"; value: controller.value("asr.alibaba.final_pass_model", "")
                                        error: controller.errorFor("asr.alibaba.final_pass_model"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.final_pass_model", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Timeout"; value: controller.value("asr.alibaba.final_pass_timeout_ms", 20000)
                                        help: "Milliseconds."; error: controller.errorFor("asr.alibaba.final_pass_timeout_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("asr.alibaba.final_pass_timeout_ms", value)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Enable ITN"; checked: controller.value("asr.alibaba.final_pass_enable_itn", false)
                                        help: "Apply inverse text normalization in the final pass."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("asr.alibaba.final_pass_enable_itn", checked)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "Output"
                                    description: "Control how recognized text reaches the focused application."
                                    SettingCombo {
                                        theme: theme; label: "Mode"; value: controller.value("output.mode", "type")
                                        labels: ["Type", "Clipboard only", "Clipboard paste"]
                                        values: ["type", "clipboard", "paste"]
                                        error: controller.errorFor("output.mode"); enabled: !controller.busy
                                        onSelected: value => controller.setValue("output.mode", value)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Fallback to clipboard"; checked: controller.value("output.fallback_to_clipboard", true)
                                        help: "Copy text when direct delivery fails."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("output.fallback_to_clipboard", checked)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Type delay"; value: controller.value("output.type_delay_ms", 0)
                                        help: "Milliseconds between generated key events."; error: controller.errorFor("output.type_delay_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("output.type_delay_ms", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Pre-type delay"; value: controller.value("output.pre_type_delay_ms", 140)
                                        help: "Milliseconds before delivery starts."; error: controller.errorFor("output.pre_type_delay_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("output.pre_type_delay_ms", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Paste keys"; value: controller.value("output.paste_keys", "shift+Insert")
                                        error: controller.errorFor("output.paste_keys"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("output.paste_keys", value)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Prefer paste for XWayland"; checked: controller.value("output.prefer_paste_for_xwayland", true)
                                        help: "Avoid garbled direct typing in XWayland clients."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("output.prefer_paste_for_xwayland", checked)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "XWayland paste keys"; value: controller.value("output.xwayland_paste_keys", "shift+Insert")
                                        error: controller.errorFor("output.xwayland_paste_keys"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("output.xwayland_paste_keys", value)
                                    }
                                }
                                SectionCard {
                                    theme: theme
                                    title: "Input method"
                                    SettingSwitch {
                                        theme: theme; label: "Manage Fcitx5"; checked: controller.value("ime.manage_fcitx5", true)
                                        help: "Coordinate output with the active Fcitx5 input method."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("ime.manage_fcitx5", checked)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Force ASCII before output"; checked: controller.value("ime.force_ascii_before_output", true)
                                        help: "Switch to ASCII mode before inserting recognized text."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("ime.force_ascii_before_output", checked)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "LLM refinement"
                                    description: "Plaintext API keys are never part of the configuration draft. Credential replacement is sent only through backend stdin."
                                    SettingSwitch {
                                        theme: theme; label: "Enable refinement"; checked: controller.value("llm.enabled", false)
                                        help: "Conservatively refine recognized transcripts."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("llm.enabled", checked)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "API base URL"; value: controller.value("llm.api_base_url", "")
                                        error: controller.errorFor("llm.api_base_url"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("llm.api_base_url", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Replace OpenRouter API key"; value: controller.openrouterCredential
                                        help: controller.credentialLabel("openrouter-api-key") + ". Blank uses the stored credential."
                                        password: true; placeholderText: "Enter a new credential"
                                        error: controller.errorFor("credentials.openrouter-api-key"); enabled: !controller.busy
                                        onEdited: value => controller.openrouterCredential = value
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Model"; value: controller.value("llm.model", "")
                                        error: controller.errorFor("llm.model"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("llm.model", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Timeout"; value: controller.value("llm.timeout_ms", 5000)
                                        help: "Milliseconds."; error: controller.errorFor("llm.timeout_ms"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("llm.timeout_ms", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Provider sort"; value: controller.value("llm.provider_sort", "")
                                        help: "Optional OpenRouter provider ordering expression."; error: controller.errorFor("llm.provider_sort"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("llm.provider_sort", value)
                                    }
                                    SettingSwitch {
                                        theme: theme; label: "Use agent context"; checked: controller.value("llm.agent_context_enabled", false)
                                        help: "Send a redacted excerpt from the focused Pi or Codex session."; enabled: !controller.busy
                                        onToggled: checked => controller.setValue("llm.agent_context_enabled", checked)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Agent context limit"; value: controller.value("llm.agent_context_max_chars", 6000)
                                        help: "Maximum characters."; error: controller.errorFor("llm.agent_context_max_chars"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("llm.agent_context_max_chars", value)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }

                            ColumnLayout {
                                spacing: 16
                                SectionCard {
                                    theme: theme
                                    title: "HUD overlay"
                                    description: "Configure visibility, geometry, placement, and keyboard nudge distance."
                                    SettingSwitch {
                                        theme: theme; label: "Enable HUD"; checked: controller.value("hud.enabled", true)
                                        enabled: !controller.busy
                                        onToggled: checked => controller.setValue("hud.enabled", checked)
                                    }
                                    SettingCombo {
                                        theme: theme; label: "Position"; value: controller.value("hud.position", "bottom-center")
                                        labels: ["Bottom center", "Bottom left", "Bottom right"]
                                        values: ["bottom-center", "bottom-left", "bottom-right"]
                                        error: controller.errorFor("hud.position"); enabled: !controller.busy
                                        onSelected: value => controller.setValue("hud.position", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Bottom margin"; value: controller.value("hud.margin_bottom", 72)
                                        help: "Pixels."; error: controller.errorFor("hud.margin_bottom"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("hud.margin_bottom", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Base height"; value: controller.value("hud.height", 56)
                                        help: "Pixels."; error: controller.errorFor("hud.height"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("hud.height", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Horizontal offset"; value: controller.value("hud.offset_x", 0)
                                        help: "Signed pixels."; error: controller.errorFor("hud.offset_x"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("hud.offset_x", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Vertical offset"; value: controller.value("hud.offset_y", 0)
                                        help: "Signed pixels."; error: controller.errorFor("hud.offset_y"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("hud.offset_y", value)
                                    }
                                    SettingTextField {
                                        theme: theme; label: "Nudge step"; value: controller.value("hud.nudge_step", 24)
                                        help: "Signed pixel adjustment per nudge command."; error: controller.errorFor("hud.nudge_step"); enabled: !controller.busy
                                        onEdited: value => controller.setValue("hud.nudge_step", value)
                                    }
                                }
                                Item { Layout.preferredHeight: 16 }
                            }
                        }
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Math.max(76, footerLayout.implicitHeight + 24)
                    color: theme.surface
                    border.color: theme.border

                    RowLayout {
                        id: footerLayout
                        anchors.fill: parent
                        anchors.margins: 12
                        spacing: 10

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 3
                            Label {
                                visible: controller.globalError.length > 0
                                text: controller.globalError
                                color: theme.error
                                font.pixelSize: 12
                                font.weight: Font.Medium
                                wrapMode: Text.WordWrap
                                Layout.fillWidth: true
                                Accessible.role: Accessible.AlertMessage
                            }
                            Label {
                                visible: controller.globalError.length === 0
                                    && controller.statusMessage.length > 0
                                text: controller.statusMessage
                                color: theme.success
                                font.pixelSize: 12
                                wrapMode: Text.WordWrap
                                Layout.fillWidth: true
                            }
                            Label {
                                visible: controller.globalError.length === 0
                                    && controller.statusMessage.length === 0
                                text: controller.busy ? "Working…"
                                    : (controller.dirty ? "Changes have not been saved." : "Configuration is up to date.")
                                color: theme.subtle
                                font.pixelSize: 12
                            }
                        }

                        AppButton {
                            theme: theme
                            text: controller.loading ? "Reloading…" : "Reload"
                            enabled: !controller.busy
                            onClicked: controller.reload()
                        }
                        AppButton {
                            theme: theme
                            text: controller.testing ? "Testing…" : "Test LLM"
                            enabled: !controller.busy
                            onClicked: controller.testLlm()
                        }
                        AppButton {
                            theme: theme
                            text: controller.saving ? "Saving…" : "Save"
                            primary: true
                            enabled: !controller.busy && controller.dirty
                            onClicked: controller.save()
                        }
                    }
                }
            }
        }

        Dialog {
            id: closeDialog
            parent: window.contentItem
            width: 440
            height: 180
            x: Math.round((parent.width - width) / 2)
            y: Math.round((parent.height - height) / 2)
            modal: true
            title: "Discard unsaved changes?"
            standardButtons: Dialog.NoButton

            background: Rectangle {
                color: theme.surface
                border.color: theme.border
                radius: 12
            }
            contentItem: ColumnLayout {
                spacing: 18
                Label {
                    text: "Your configuration or credential replacements have not been saved."
                    color: theme.foreground
                    wrapMode: Text.WordWrap
                    Layout.preferredWidth: 380
                }
                RowLayout {
                    Layout.alignment: Qt.AlignRight
                    AppButton { theme: theme; text: "Keep editing"; onClicked: closeDialog.close() }
                    AppButton {
                        theme: theme; text: "Discard and close"; danger: true
                        onClicked: {
                            closeDialog.close();
                            Qt.quit();
                        }
                    }
                }
            }
        }
    }
}
