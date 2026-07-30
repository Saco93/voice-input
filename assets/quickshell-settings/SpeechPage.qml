import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller
    property alias advancedExpanded: speechAdvanced.expanded

    title: "Speech"
    description: "Configure audio capture and speech recognition."

    SettingsGrid {
        id: speechGrid

        SectionCard {
            theme: root.theme
            title: "Audio capture"
            description: "Recording source and capture window."

            SettingTextField {
                theme: root.theme
                label: "Device"
                value: root.controller.value("audio.device", "default")
                error: root.controller.errorFor("audio.device")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("audio.device", value);
                }
            }

            SettingTextField {
                theme: root.theme
                label: "Maximum duration"
                value: root.controller.value("audio.max_duration_secs", 90)
                help: "Seconds."
                error: root.controller.errorFor("audio.max_duration_secs")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("audio.max_duration_secs", value);
                }
            }

            SettingSwitch {
                theme: root.theme
                label: "Enable pre-roll"
                checked: root.controller.value("audio.pre_roll_enabled", false)
                help: "Keep a short capture buffer so the first syllable is preserved."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("audio.pre_roll_enabled", checked);
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Recognition"
            description: "Recognition provider, language, and fallback."

            SettingCombo {
                theme: root.theme
                label: "Provider"
                value: root.controller.value("asr.provider", "local-cli")
                labels: ["Local CLI", "Alibaba Qwen realtime"]
                values: ["local-cli", "alibaba-qwen-realtime"]
                error: root.controller.errorFor("asr.provider")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("asr.provider", value);
                }
            }

            SettingCombo {
                theme: root.theme
                label: "Language"
                value: root.controller.value("asr.language", "simplified-chinese")
                labels: ["English", "Simplified Chinese", "Traditional Chinese", "Japanese", "Korean"]
                values: ["english", "simplified-chinese", "traditional-chinese", "japanese", "korean"]
                error: root.controller.errorFor("asr.language")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("asr.language", value);
                }
            }

            SettingSwitch {
                theme: root.theme
                label: "Fallback to local"
                checked: root.controller.value("asr.fallback_to_local", true)
                help: "Use local recognition when remote recognition fails."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("asr.fallback_to_local", checked);
                }
            }

        }

        SectionCard {
            visible: root.controller.value("asr.provider", "local-cli") === "local-cli" || root.controller.value("asr.fallback_to_local", true) || root.controller.hasErrorPrefix("asr.backend_command") || root.controller.hasErrorPrefix("asr.engine") || root.controller.hasErrorPrefix("asr.model")
            theme: root.theme
            title: "Local recognition"
            description: "Local CLI backend used as the provider or fallback."

            SettingTextField {
                theme: root.theme
                label: "Backend command"
                value: root.controller.value("asr.backend_command", "")
                help: "Executable used by local recognition."
                error: root.controller.errorFor("asr.backend_command")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("asr.backend_command", value);
                }
            }

            SettingTextField {
                theme: root.theme
                label: "Local engine"
                value: root.controller.value("asr.engine", "")
                error: root.controller.errorFor("asr.engine")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("asr.engine", value);
                }
            }

            SettingTextField {
                theme: root.theme
                label: "Local model"
                value: root.controller.value("asr.model", "")
                placeholderText: "Backend default"
                error: root.controller.errorFor("asr.model")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("asr.model", value);
                }
            }

        }

        SectionCard {
            visible: root.controller.value("asr.provider", "local-cli") === "alibaba-qwen-realtime" || root.controller.hasErrorPrefix("asr.alibaba.") || root.controller.errorFor("credentials.alibaba-api-key").length > 0
            theme: root.theme
            title: "Alibaba realtime"
            description: "Credential and realtime recognition behavior."

            SettingTextField {
                theme: root.theme
                label: "Replace Alibaba API key"
                value: root.controller.alibabaCredential
                help: root.controller.credentialLabel("alibaba-api-key") + ". Blank keeps it unchanged."
                password: true
                placeholderText: "Enter a new credential"
                error: root.controller.errorFor("credentials.alibaba-api-key")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    root.controller.alibabaCredential = value;
                    root.controller.clearFieldError("credentials.alibaba-api-key");
                    root.controller.statusMessage = "";
                }
            }

            SettingTextField {
                theme: root.theme
                label: "Realtime model"
                value: root.controller.value("asr.alibaba.model", "")
                error: root.controller.errorFor("asr.alibaba.model")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("asr.alibaba.model", value);
                }
            }

            SettingCombo {
                theme: root.theme
                label: "Turn mode"
                value: root.controller.value("asr.alibaba.turn_mode", "server-vad")
                labels: ["Server VAD", "Manual commit"]
                values: ["server-vad", "manual"]
                error: root.controller.errorFor("asr.alibaba.turn_mode")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("asr.alibaba.turn_mode", value);
                }
            }

        }

    }

    AdvancedSection {
        id: speechAdvanced

        theme: root.theme
        description: "Capture cadence, timeouts, and Alibaba tuning."

        SettingsGrid {
            SectionCard {
                theme: root.theme
                title: "Audio tuning"

                SettingTextField {
                    theme: root.theme
                    label: "Sample rate"
                    value: root.controller.value("audio.sample_rate", 16000)
                    help: "Samples per second (Hz)."
                    error: root.controller.errorFor("audio.sample_rate")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("audio.sample_rate", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Partial interval"
                    value: root.controller.value("audio.partial_interval_ms", 1500)
                    help: "Milliseconds between partial updates."
                    error: root.controller.errorFor("audio.partial_interval_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("audio.partial_interval_ms", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Pre-roll window"
                    value: root.controller.value("audio.pre_roll_ms", 500)
                    help: "Milliseconds retained before activation."
                    error: root.controller.errorFor("audio.pre_roll_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("audio.pre_roll_ms", value);
                    }
                }

            }

            SectionCard {
                theme: root.theme
                title: "Recognition timeouts"

                SettingTextField {
                    theme: root.theme
                    label: "Connect timeout"
                    value: root.controller.value("asr.connect_timeout_ms", 5000)
                    help: "Milliseconds."
                    error: root.controller.errorFor("asr.connect_timeout_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.connect_timeout_ms", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Finalize timeout"
                    value: root.controller.value("asr.finalize_timeout_ms", 8000)
                    help: "Milliseconds."
                    error: root.controller.errorFor("asr.finalize_timeout_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.finalize_timeout_ms", value);
                    }
                }

            }

            SectionCard {
                visible: root.controller.value("asr.provider", "local-cli") === "alibaba-qwen-realtime" || root.controller.hasErrorPrefix("asr.alibaba.")
                theme: root.theme
                title: "Alibaba tuning"

                SettingTextField {
                    theme: root.theme
                    label: "Endpoint"
                    value: root.controller.value("asr.alibaba.endpoint", "")
                    error: root.controller.errorFor("asr.alibaba.endpoint")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.alibaba.endpoint", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "VAD threshold"
                    value: root.controller.value("asr.alibaba.vad_threshold", 0.2)
                    error: root.controller.errorFor("asr.alibaba.vad_threshold")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.alibaba.vad_threshold", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Silence duration"
                    value: root.controller.value("asr.alibaba.silence_duration_ms", 400)
                    help: "Milliseconds."
                    error: root.controller.errorFor("asr.alibaba.silence_duration_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.alibaba.silence_duration_ms", value);
                    }
                }

            }

            SectionCard {
                visible: root.controller.value("asr.provider", "local-cli") === "alibaba-qwen-realtime" || root.controller.hasErrorPrefix("asr.alibaba.final_pass_")
                theme: root.theme
                title: "Alibaba final pass"

                SettingSwitch {
                    theme: root.theme
                    label: "Enable final pass"
                    checked: root.controller.value("asr.alibaba.final_pass_enabled", false)
                    enabled: !root.controller.busy
                    onToggled: (checked) => {
                        return root.controller.setValue("asr.alibaba.final_pass_enabled", checked);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Base URL"
                    value: root.controller.value("asr.alibaba.final_pass_base_url", "")
                    error: root.controller.errorFor("asr.alibaba.final_pass_base_url")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.alibaba.final_pass_base_url", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Model"
                    value: root.controller.value("asr.alibaba.final_pass_model", "")
                    error: root.controller.errorFor("asr.alibaba.final_pass_model")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.alibaba.final_pass_model", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Timeout"
                    value: root.controller.value("asr.alibaba.final_pass_timeout_ms", 20000)
                    help: "Milliseconds."
                    error: root.controller.errorFor("asr.alibaba.final_pass_timeout_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("asr.alibaba.final_pass_timeout_ms", value);
                    }
                }

                SettingSwitch {
                    theme: root.theme
                    label: "Enable ITN"
                    checked: root.controller.value("asr.alibaba.final_pass_enable_itn", false)
                    help: "Apply inverse text normalization."
                    enabled: !root.controller.busy
                    onToggled: (checked) => {
                        return root.controller.setValue("asr.alibaba.final_pass_enable_itn", checked);
                    }
                }

            }

        }

    }

}
