import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller
    property alias advancedExpanded: refinementAdvanced.expanded

    title: "Refinement"
    description: "Optionally refine recognized transcripts with an LLM."

    SettingsGrid {
        SectionCard {
            theme: root.theme
            title: "Refinement"

            SettingSwitch {
                theme: root.theme
                label: "Enable refinement"
                checked: root.controller.value("llm.enabled", false)
                help: "Conservatively refine recognized transcripts."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("llm.enabled", checked);
                }
            }

            SettingTextField {
                theme: root.theme
                label: "Model"
                value: root.controller.value("llm.model", "")
                placeholderText: "Provider default"
                error: root.controller.errorFor("llm.model")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.controller.setValue("llm.model", value);
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Credential"
            description: "The replacement is sent only to the backend and is never copied into the draft."

            SettingTextField {
                theme: root.theme
                label: "Replace OpenRouter API key"
                value: root.controller.openrouterCredential
                help: root.controller.credentialLabel("openrouter-api-key") + ". Blank uses the stored credential."
                password: true
                placeholderText: "Enter a new credential"
                error: root.controller.errorFor("credentials.openrouter-api-key")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    root.controller.openrouterCredential = value;
                    root.controller.clearFieldError("credentials.openrouter-api-key");
                    root.controller.statusMessage = "";
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Context"

            SettingSwitch {
                theme: root.theme
                label: "Use agent context"
                checked: root.controller.value("llm.agent_context_enabled", false)
                help: "Locally segment a redacted Pi or Codex excerpt and send only bounded, deduplicated terminology."
                enabled: !root.controller.busy
                onToggled: (checked) => {
                    return root.controller.setValue("llm.agent_context_enabled", checked);
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Test refinement"
            description: "Test the current LLM draft and credential without saving it."

            AppButton {
                theme: root.theme
                text: root.controller.testing ? "Testing…" : "Test LLM"
                enabled: !root.controller.busy
                onClicked: root.controller.testLlm()
            }

        }

    }

    AdvancedSection {
        id: refinementAdvanced

        theme: root.theme
        description: "Endpoint, timeout, provider ordering, and context limits."

        SettingsGrid {
            SectionCard {
                theme: root.theme
                title: "Provider settings"

                SettingTextField {
                    theme: root.theme
                    label: "API base URL"
                    value: root.controller.value("llm.api_base_url", "")
                    error: root.controller.errorFor("llm.api_base_url")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("llm.api_base_url", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Timeout"
                    value: root.controller.value("llm.timeout_ms", 15000)
                    help: "Milliseconds."
                    error: root.controller.errorFor("llm.timeout_ms")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("llm.timeout_ms", value);
                    }
                }

                SettingTextField {
                    theme: root.theme
                    label: "Provider sort"
                    value: root.controller.value("llm.provider_sort", "")
                    help: "Optional OpenRouter provider ordering expression."
                    error: root.controller.errorFor("llm.provider_sort")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("llm.provider_sort", value);
                    }
                }

            }

            SectionCard {
                theme: root.theme
                title: "Agent context"
                showDivider: false

                SettingTextField {
                    theme: root.theme
                    label: "Context limit"
                    value: root.controller.value("llm.agent_context_max_chars", 6000)
                    help: "Maximum redacted agent-session characters (500–12000)."
                    error: root.controller.errorFor("llm.agent_context_max_chars")
                    enabled: !root.controller.busy
                    onEdited: (value) => {
                        return root.controller.setValue("llm.agent_context_max_chars", value);
                    }
                }

            }

        }

    }

}
