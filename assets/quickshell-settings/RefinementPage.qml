import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

SettingsPage {
    id: root

    required property SettingsController controller
    property alias advancedExpanded: refinementAdvanced.expanded
    property bool customModel: false
    readonly property string credentialId: controller.value("llm.credential_id", "openrouter-api-key")
    readonly property string currentModel: controller.value("llm.model", "")
    readonly property bool qwenModel: currentModel === "qwen3.8-27b"
    readonly property bool gptOssModel: currentModel === "openai/gpt-oss-120b"
    readonly property var reasoningValues: qwenModel ? ["none", "low", "medium", "xhigh"] : (gptOssModel ? ["low", "medium", "high"] : [""])
    readonly property var reasoningLabels: qwenModel ? ["Off", "Low", "Medium", "XHigh"] : (gptOssModel ? ["Low", "Medium", "High"] : ["Provider default"])
    readonly property string reasoningEffort: controller.value("llm.reasoning_effort", "") || (qwenModel ? "none" : (gptOssModel ? "medium" : ""))
    readonly property string modelPreset: {
        if (customModel)
            return "custom";

        const model = controller.value("llm.model", "");
        const endpoint = controller.value("llm.api_base_url", "").replace(/\/+$/, "");
        if (model === "openai/gpt-oss-120b" && endpoint === "https://openrouter.ai/api/v1" && credentialId === "openrouter-api-key")
            return "gpt-oss-120b";

        if (model === "qwen3.8-27b" && endpoint === "https://dashscope.aliyuncs.com/compatible-mode/v1" && credentialId === "alibaba-api-key")
            return "qwen3.8-27b";

        return "custom";
    }

    function selectModelPreset(preset) {
        customModel = preset === "custom";
        if (customModel)
            return ;

        const qwen = preset === "qwen3.8-27b";
        setModel(qwen ? "qwen3.8-27b" : "openai/gpt-oss-120b");
        controller.setValue("llm.api_base_url", qwen ? "https://dashscope.aliyuncs.com/compatible-mode/v1" : "https://openrouter.ai/api/v1");
        controller.setValue("llm.credential_id", qwen ? "alibaba-api-key" : "openrouter-api-key");
    }

    function setModel(model) {
        if (controller.value("llm.model", "") !== model)
            controller.setValue("llm.reasoning_effort", "");

        controller.setValue("llm.model", model);
    }

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

            SettingCombo {
                theme: root.theme
                label: "Model preset"
                value: root.modelPreset
                labels: ["GPT OSS 120B (OpenRouter)", "Qwen3.8 27B (Alibaba Beijing)", "Custom"]
                values: ["gpt-oss-120b", "qwen3.8-27b", "custom"]
                help: "Select a model preset or configure a custom model."
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.selectModelPreset(value);
                }
            }

            SettingTextField {
                visible: root.modelPreset === "custom" || root.controller.errorFor("llm.model").length > 0
                theme: root.theme
                label: "Model"
                value: root.controller.value("llm.model", "")
                placeholderText: "Provider default"
                error: root.controller.errorFor("llm.model")
                enabled: !root.controller.busy
                onEdited: (value) => {
                    return root.setModel(value);
                }
            }

            SettingCombo {
                theme: root.theme
                label: "Reasoning level"
                value: root.reasoningEffort
                labels: root.reasoningLabels
                values: root.reasoningValues
                help: root.qwenModel ? "Off disables Qwen reasoning for the fastest refinement." : (root.gptOssModel ? "GPT OSS supports Low, Medium, and High; reasoning cannot be turned off." : "This custom model uses the provider's default reasoning settings.")
                error: root.controller.errorFor("llm.reasoning_effort")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("llm.reasoning_effort", value);
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Credential"
            description: "The replacement is sent only to the backend and is never copied into the draft."

            SettingCombo {
                visible: root.modelPreset === "custom" || root.controller.errorFor("llm.credential_id").length > 0
                theme: root.theme
                label: "API credential"
                value: root.credentialId
                labels: ["OpenRouter", "Alibaba"]
                values: ["openrouter-api-key", "alibaba-api-key"]
                error: root.controller.errorFor("llm.credential_id")
                enabled: !root.controller.busy
                onSelected: (value) => {
                    return root.controller.setValue("llm.credential_id", value);
                }
            }

            SettingTextField {
                theme: root.theme
                label: root.credentialId === "alibaba-api-key" ? "Replace Alibaba API key" : "Replace OpenRouter API key"
                value: root.credentialId === "alibaba-api-key" ? root.controller.alibabaCredential : root.controller.openrouterCredential
                help: root.controller.credentialLabel(root.credentialId) + ". Blank uses the stored credential."
                password: true
                placeholderText: "Enter a new credential"
                error: root.controller.errorFor("credentials." + root.credentialId)
                enabled: !root.controller.busy
                onEdited: (value) => {
                    if (root.credentialId === "alibaba-api-key")
                        root.controller.alibabaCredential = value;
                    else
                        root.controller.openrouterCredential = value;
                    root.controller.clearFieldError("credentials." + root.credentialId);
                    root.controller.statusMessage = "";
                }
            }

        }

        SectionCard {
            theme: root.theme
            title: "Context"

            SettingSwitch {
                theme: root.theme
                label: "Use Pi/Codex session terminology"
                checked: root.controller.value("llm.agent_context_enabled", false)
                help: "At dictation start, locally extract rare-first terminology for Audio3 Session Context and Refine."
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
                title: "Session terminology"
                showDivider: false

                SettingTextField {
                    theme: root.theme
                    label: "Context limit"
                    value: root.controller.value("llm.agent_context_max_chars", 6000)
                    help: "Maximum redacted source characters before local terminology extraction (500–12000)."
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
