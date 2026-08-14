import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    // A UI preference failure must never affect the settings window.
    // Keep the system-language fallback for malformed preferences.

    id: root

    readonly property string preferencePath: {
        const xdgConfig = Quickshell.env("XDG_CONFIG_HOME");
        const configRoot = xdgConfig && xdgConfig.length > 0 ? xdgConfig : Quickshell.env("HOME") + "/.config";
        return configRoot + "/voice-input/settings-ui.json";
    }
    readonly property string systemLocale: {
        const candidates = [Quickshell.env("LC_ALL"), Quickshell.env("LC_MESSAGES"), Quickshell.env("LANG"), Qt.locale().name];
        for (let i = 0; i < candidates.length; ++i) {
            const candidate = String(candidates[i] || "").toLowerCase();
            if (candidate.length > 0)
                return candidate.indexOf("zh") === 0 ? "zh-CN" : "en";

        }
        return "en";
    }
    property string locale: systemLocale
    property bool explicitChoice: false
    readonly property var zh: ({
        "Voice Input": "语音输入",
        "Overview": "概览",
        "Speech": "语音识别",
        "Refinement": "文本优化",
        "Output": "输出",
        "Appearance": "外观",
        "Advanced": "高级设置",
        "Hotkey & state": "快捷键与状态",
        "Settings destinations": "设置页面",
        "Local service": "本地服务",
        "Checking local service status…": "正在检查本地服务状态…",
        "Runtime status is unavailable.": "运行状态不可用。",
        "Runtime status is temporarily unavailable.": "运行状态暂时不可用。",
        "Running": "正在运行",
        "Not running": "未运行",
        "Service state was not reported.": "未报告服务状态。",
        "No runtime details reported.": "未报告运行详情。",
        "Runtime details are unavailable.": "运行详情不可用。",
        "Voice Input service": "语音输入服务",
        "Unsaved": "未保存",
        "More actions": "更多操作",
        "Reloading…": "正在重新加载…",
        "Reload settings": "重新加载设置",
        "Close": "关闭",
        "English": "英语",
        "Simplified Chinese": "简体中文",
        "Traditional Chinese": "繁体中文",
        "Japanese": "日语",
        "Korean": "韩语",
        "Change settings language": "更改设置语言",
        "errors": "个错误",
        "Service status and current configuration.": "查看服务状态和当前配置。",
        "Current configuration": "当前配置",
        "Enabled": "已启用",
        "Disabled": "已禁用",
        "Skipped": "已跳过",
        "Pending": "待应用",
        "Alibaba Qwen realtime": "Alibaba Qwen 实时识别",
        "Qwen-Audio-3 (experimental)": "Qwen-Audio-3（实验性）",
        "Unknown provider": "未知提供商",
        "Local CLI": "本地 CLI",
        "Clipboard only": "仅剪贴板",
        "Clipboard paste": "通过剪贴板粘贴",
        "Direct typing": "直接键入",
        "Type": "键入",
        "Model: provider default": "模型：使用提供商默认值",
        "Coordinates with Fcitx5": "与 Fcitx5 协同",
        "Leaves Fcitx5 unchanged": "不更改 Fcitx5",
        "HUD visible": "HUD 可见",
        "HUD hidden": "HUD 隐藏",
        "Configure audio capture and speech recognition.": "配置音频采集和语音识别。",
        "Audio capture": "音频采集",
        "Recording source and capture window.": "设置录音来源和采集时长。",
        "Device": "设备",
        "Maximum duration": "最长时长",
        "Seconds.": "秒。",
        "Recording finishes automatically at this limit, in seconds.": "达到此秒数后会自动结束录音。",
        "Enable pre-roll": "启用预录",
        "Keep a short capture buffer so the first syllable is preserved.": "保留一段短暂的采集缓冲，以免遗漏第一个音节。",
        "Recognition": "语音识别",
        "Recognition provider, language, and fallback.": "设置识别提供商、语言和备用方案。",
        "Provider": "提供商",
        "Language": "语言",
        "When Audio3 language hints are enabled, this selection guides recognition; Chinese, Japanese, and Korean also retain English mixing.": "启用 Audio3 语言提示后，此选项会引导识别；中文、日语和韩语还会保留英语混合识别。",
        "Fallback to local": "失败时使用本地识别",
        "Use local recognition when remote recognition fails.": "远程识别失败时使用本地识别。",
        "Local recognition": "本地识别",
        "Local CLI backend used as the provider or fallback.": "作为主要提供商或备用方案的本地 CLI 后端。",
        "Backend command": "后端命令",
        "Executable used by local recognition.": "本地识别使用的可执行文件。",
        "Local engine": "本地引擎",
        "Local model": "本地模型",
        "Backend default": "使用后端默认值",
        "Alibaba credential": "Alibaba 凭据",
        "Shared by Alibaba realtime and Qwen-Audio-3.": "Alibaba 实时识别和 Qwen-Audio-3 共用此凭据。",
        "Alibaba realtime": "Alibaba 实时识别",
        "Credential and realtime recognition behavior.": "设置凭据和实时识别行为。",
        "Realtime recognition behavior.": "设置实时识别行为。",
        "Replace Alibaba API key": "替换 Alibaba API key",
        "Enter a new credential": "输入新凭据",
        "API keys are region-scoped; Voice Input never probes another region or migrates a key automatically.": "API key 受区域范围约束；语音输入绝不会探测其他区域，也不会自动迁移 key。",
        "Realtime model": "实时模型",
        "Turn mode": "分段模式",
        "Server VAD": "服务端 VAD",
        "Manual commit": "手动提交",
        "Experimental provider; behavior and API compatibility may change.": "此提供商仍处于实验阶段，其行为和 API 兼容性可能发生变化。",
        "I understand and enable experimental Qwen-Audio-3": "我了解相关风险并启用实验性 Qwen-Audio-3",
        "Selecting the provider does not enable this acknowledgement.": "仅选择此提供商不会自动确认并启用实验功能。",
        "Endpoint mode": "端点模式",
        "Regional": "区域路由",
        "Regional routing uses the fixed reviewed Alibaba host for the selected region. Custom keeps the exact streaming and native endpoints in Advanced settings.": "区域路由使用所选区域固定且经过审核的 Alibaba 主机。自定义模式会在高级设置中保留原样的流式端点和原生端点。",
        "Region": "区域",
        "Beijing": "北京",
        "Singapore": "新加坡",
        "Select the region that owns the configured Alibaba API key. Singapore controls still require scenario-specific live validation.": "请选择当前 Alibaba API key 所属的区域。新加坡区域的各项控制仍需按具体场景完成在线验证。",
        "Enable language hints": "启用语言提示",
        "Opt in to sending the selected language as an Audio3 recognition hint.": "选择将所选语言作为 Audio3 识别提示发送。",
        "Enable streaming heartbeat": "启用流式 heartbeat",
        "Opt in to keeping long silent push-to-talk sessions alive while audio frames continue.": "选择在音频帧持续发送时，使长时间静音的按键说话 session 保持连接。",
        "Recognition preset": "识别预设",
        "Standard": "标准",
        "Low-latency dictation": "低延迟听写",
        "Long-form": "长篇语音",
        "Custom": "自定义",
        "Standard preserves existing behavior. Low-latency dictation and long-form are evaluation candidates pending live pause and noise validation. Custom exposes every raw recognition control.": "标准预设会保留现有行为。低延迟听写和长篇语音是评估候选值，仍需完成暂停和噪声的在线验证。自定义预设会显示所有原始识别控制项。",
        "Dynamic vocabulary": "动态词汇表",
        "Optional global Audio3 terms. Enter one JSON object per line with term and weight; weights are 1–5 or 50. Every listed term is sent remotely.": "可选的全局 Audio3 词条。每行输入一个包含 term 和 weight 的 JSON 对象；权重可以是 1–5 或 50。列出的每个词条都会发送给远程服务。",
        "{\"term\":\"Voice Input\",\"weight\":5}": "{\"term\":\"语音输入\",\"weight\":5}",
        "Native final pass": "原生最终处理",
        "Streaming only": "仅流式识别",
        "Adaptive": "自适应",
        "Always": "始终运行",
        "Adaptive runs native recognition for unhealthy, incomplete, overloaded, or 30-second recordings. Always requests maximum accuracy for every nonempty recording.": "自适应模式会在流式识别异常、未明确完成、过载或录音达到 30 秒时运行原生识别。始终运行模式会针对每段非空录音请求最高准确度。",
        "Technical settings for this page.": "此页面的技术设置。",
        "Capture cadence, timeouts, and Alibaba tuning.": "设置采集频率、超时和 Alibaba 调优参数。",
        "Capture cadence, timeouts, and provider tuning.": "设置采集频率、超时和提供商调优参数。",
        "Show advanced settings": "显示高级设置",
        "Hide advanced settings": "隐藏高级设置",
        "Show": "显示",
        "Hide": "隐藏",
        "Audio tuning": "音频调优",
        "Sample rate": "采样率",
        "Samples per second (Hz).": "每秒采样数（Hz）。",
        "Partial interval": "中间结果间隔",
        "Milliseconds between partial updates.": "中间结果更新之间的毫秒数。",
        "Pre-roll window": "预录窗口",
        "Milliseconds retained before activation.": "激活前保留的毫秒数。",
        "Recognition timeouts": "识别超时",
        "Connect timeout": "连接超时",
        "Finalize timeout": "最终结果超时",
        "Milliseconds.": "毫秒。",
        "Alibaba tuning": "Alibaba 调优",
        "Endpoint": "端点",
        "VAD threshold": "VAD 阈值",
        "Silence duration": "静音时长",
        "Alibaba final pass": "Alibaba 最终处理",
        "Enable final pass": "启用最终处理",
        "Base URL": "基础 URL",
        "Model": "模型",
        "Timeout": "超时",
        "Enable ITN": "启用 ITN",
        "Apply inverse text normalization.": "应用逆文本规范化。",
        "Qwen-Audio-3 streaming": "Qwen-Audio-3 流式识别",
        "Streaming endpoint": "流式识别端点",
        "Streaming model": "流式识别模型",
        "Maximum sentence silence": "最大分句静音时长",
        "Milliseconds from 200 to 6000. Lower values finalize speech segments sooner.": "范围为 200 到 6000 毫秒。较小的值会更快完成语音分段。",
        "Enable semantic punctuation": "启用语义标点",
        "Allow the streaming model to use semantic punctuation when finalizing speech segments.": "允许流式模型在完成语音分段时使用语义标点。",
        "Enable multi-threshold mode": "启用多阈值模式",
        "Use the documented adaptive VAD threshold mode. It cannot be combined with semantic punctuation.": "使用文档说明的自适应 VAD 阈值模式。此模式不能与语义标点同时启用。",
        "Override speech/noise threshold": "覆盖语音/噪声阈值",
        "Send an optional threshold from -1 to 1. Lower values classify more noise as speech; Alibaba publishes no default.": "发送一个 -1 到 1 的可选阈值。数值越低，越多噪声会被归类为语音；Alibaba 未公布默认值。",
        "Speech/noise threshold": "语音/噪声阈值",
        "Finite value from -1 to 1. This value is sent remotely only while the override is enabled.": "请输入 -1 到 1 的有限数值。仅在启用覆盖时，程序才会将该值发送给远程服务。",
        "Enter a finite number.": "请输入有限数值。",
        "Qwen-Audio-3 native final pass": "Qwen-Audio-3 原生最终处理",
        "Native endpoint": "原生端点",
        "Native model": "原生模型",
        "Native timeout": "原生处理超时",
        "Optionally refine recognized transcripts with an LLM.": "可以选择使用 LLM 优化识别后的文本。",
        "Enable refinement": "启用文本优化",
        "Conservatively refine recognized transcripts.": "以保守方式优化识别后的文本。",
        "Provider default": "使用提供商默认值",
        "Credential": "凭据",
        "The replacement is sent only to the backend and is never copied into the draft.": "替换值只发送到后端，不会复制到设置草稿中。",
        "Replace OpenRouter API key": "替换 OpenRouter API key",
        "Context": "上下文",
        "Use Pi/Codex session terminology": "使用 Pi/Codex Session 术语",
        "At dictation start, locally extract rare-first terminology for Audio3 Session Context and Refine.": "开始听写时，在本地提取低频优先的术语，并将其分别用于 Audio3 Session Context 和 Refine。",
        "Test refinement": "测试文本优化",
        "Test the current LLM draft and credential without saving it.": "无需保存即可测试当前 LLM 设置草稿和凭据。",
        "Testing…": "正在测试…",
        "Test LLM": "测试 LLM",
        "Endpoint, timeout, provider ordering, and context limits.": "设置端点、超时、提供商顺序和上下文限制。",
        "Provider settings": "提供商设置",
        "API base URL": "API 基础 URL",
        "Provider sort": "提供商顺序",
        "Optional OpenRouter provider ordering expression.": "可选的 OpenRouter 提供商顺序表达式。",
        "Session terminology": "Session 术语",
        "Context limit": "上下文限制",
        "Maximum redacted source characters before local terminology extraction (500–12000).": "本地提取术语前最多读取 500–12000 个经过脱敏的源字符。",
        "Clipboard delivery and input-method coordination.": "控制剪贴板粘贴和输入法协同。",
        "Delivery": "输出方式",
        "Mode": "模式",
        "All recognized text is pasted without synthetic character keycodes.": "所有识别文本均通过粘贴输入，不生成逐字符合成 keycode。",
        "Input method": "输入法",
        "Manage Fcitx5": "管理 Fcitx5",
        "Coordinate output with the active Fcitx5 input method.": "输出时与当前 Fcitx5 输入法协同。",
        "Force ASCII before output": "输出前强制切换到 ASCII",
        "Switch to ASCII mode before inserting recognized text.": "插入识别文本前切换到 ASCII 模式。",
        "Delivery timing and paste shortcuts.": "设置输出时序和粘贴快捷键。",
        "Timing and keys": "时序和按键",
        "Pre-output delay": "输出前延迟",
        "Milliseconds before delivery starts.": "开始输出前等待的毫秒数。",
        "Paste keys": "粘贴按键",
        "XWayland paste keys": "XWayland 粘贴按键",
        "Control HUD visibility and placement.": "控制 HUD 的可见性和位置。",
        "Enable HUD": "启用 HUD",
        "Position": "位置",
        "Bottom center": "底部居中",
        "Bottom left": "左下角",
        "Bottom right": "右下角",
        "HUD geometry, offsets, and keyboard nudge distance.": "设置 HUD 尺寸、偏移和键盘微调距离。",
        "Geometry": "尺寸和位置",
        "Bottom margin": "底部边距",
        "Pixels.": "像素。",
        "Base height": "基础高度",
        "Horizontal offset": "水平偏移",
        "Signed pixels.": "带正负号的像素值。",
        "Adjustment": "调整",
        "Vertical offset": "垂直偏移",
        "Nudge step": "微调步长",
        "Signed pixel adjustment per nudge command.": "每次微调命令移动的带正负号像素值。",
        "Configure the global trigger and state storage.": "配置全局触发方式和状态存储。",
        "Hotkey": "快捷键",
        "Accelerator": "快捷键组合",
        "Hyprland-style modifier and key description.": "Hyprland 格式的修饰键和按键描述。",
        "Hold": "按住",
        "Toggle": "切换",
        "State": "状态",
        "State file": "状态文件",
        "Use auto, disabled, or an absolute custom path.": "使用 auto、disabled 或自定义绝对路径。",
        "Saving configuration and restarting service…": "正在保存配置并重启服务…",
        "Reloading configuration…": "正在重新加载配置…",
        "Testing LLM settings…": "正在测试 LLM 设置…",
        "Changes have not been saved.": "更改尚未保存。",
        "Configuration is up to date.": "配置已是最新状态。",
        "Discard": "放弃更改",
        "Saving…": "正在保存…",
        "Save & restart": "保存并重启",
        "Discard unsaved changes?": "要放弃未保存的更改吗？",
        "Your configuration or credential replacements have not been saved.": "配置更改或凭据替换尚未保存。",
        "Keep editing": "继续编辑",
        "Discard and close": "放弃并关闭",
        "Not configured": "未配置",
        "Configured": "已配置",
        "Blank keeps it unchanged.": "留空将保持不变。",
        "Blank uses the stored credential.": "留空将使用已存储的凭据。",
        "Settings backend is not running.": "设置后端未运行。",
        "VOICE_INPUT_BIN is not set; cannot start the settings backend.": "未设置 VOICE_INPUT_BIN，无法启动设置后端。",
        "Fix the highlighted numeric fields before continuing.": "请先修正突出显示的数值字段。",
        "Fix the highlighted fields before continuing.": "请先修正突出显示的字段。",
        "Invalid value.": "值无效。",
        "The settings backend returned malformed JSON.": "设置后端返回了格式错误的 JSON。",
        "The settings backend returned an invalid response.": "设置后端返回了无效响应。",
        "The settings backend rejected the request.": "设置后端拒绝了请求。",
        "Settings loaded.": "设置已加载。",
        "Settings were saved with additional errors.": "设置已保存，但还出现了其他错误。",
        "Saved and restarted Voice Input.": "已保存设置并重启语音输入。",
        "LLM connection test succeeded.": "LLM 连接测试成功。",
        "Set VOICE_INPUT_BIN to the Voice Input executable path.": "请将 VOICE_INPUT_BIN 设置为语音输入可执行文件的路径。",
        "The settings backend reported an internal error.": "设置后端报告了内部错误。",
        "request line exceeds 1 MiB": "请求行超过 1 MiB",
        "request line must contain JSON": "请求行必须包含 JSON",
        "invalid request envelope": "请求封装无效",
        "only protocol version 1 is supported": "仅支持协议版本 1",
        "invalid settings.save parameters": "settings.save 参数无效",
        "invalid llm.test parameters": "llm.test 参数无效",
        "unknown method": "未知方法",
        "configuration could not be loaded": "无法加载配置",
        "must not contain api_key fields": "不得包含 api_key 字段",
        "configuration validation failed": "配置验证失败",
        "config does not match the settings schema": "配置与设置 schema 不匹配",
        "configuration changed since it was loaded": "配置在加载后已被其他程序更改",
        "unsupported credential ID": "不支持的凭据 ID",
        "replacement credential is required": "必须提供替换凭据",
        "invalid credential value": "凭据值无效",
        "invalid credential action": "凭据操作无效",
        "unknown credential action": "未知凭据操作",
        "credential status could not be read": "无法读取凭据状态",
        "legacy credential could not be migrated; configuration was not saved": "无法迁移旧凭据；配置未保存",
        "credential could not be migrated": "无法迁移凭据",
        "credential could not be updated": "无法更新凭据",
        "voice-input.service could not be restarted": "无法重启 voice-input.service",
        "Configuration saved; service restarted": "配置已保存；服务已重启",
        "Configuration saved": "配置已保存",
        "Configuration saved, but one or more credentials could not be updated": "配置已保存，但一个或多个凭据无法更新",
        "Configuration saved, but the service could not be restarted": "配置已保存，但服务无法重启",
        "Configuration saved, but credential updates and the service restart had errors": "配置已保存，但更新凭据和重启服务时出现错误",
        "entered credential value is required": "必须提供输入的凭据值",
        "stored credential could not be read": "无法读取已存储的凭据",
        "invalid credential source": "凭据来源无效",
        "LLM connectivity test failed": "LLM 连接测试失败",
        "params must be an empty object": "params 必须是空对象",
        "configuration could not be saved": "无法保存配置",
        "is required": "必填",
        "must not contain control characters": "不得包含控制字符",
        "must be a valid URL": "必须是有效的 URL",
        "must not contain embedded credentials": "不得包含嵌入式凭据",
        "must include a host": "必须包含主机名",
        "must use transport encryption unless the host is loopback": "除回环主机外，必须使用加密传输",
        "must be a valid DNS label for hostname transport": "必须是可用于主机名传输的有效 DNS label",
        "must be true when the experimental provider is selected": "选择实验性提供商时必须明确启用此选项",
        "must not exceed maximum recording duration": "不得超过最长录音时长",
        "must be finite and between 0 and 1": "必须是 0 到 1 之间的有限数值",
        "active": "活动",
        "activating": "正在启动",
        "deactivating": "正在停止",
        "inactive": "未活动",
        "failed": "失败",
        "stopped": "已停止",
        "unknown": "未知",
        "idle": "空闲",
        "arming": "正在准备",
        "recording": "正在录音",
        "transcribing": "正在转写",
        "refining": "正在优化",
        "outputting": "正在输出",
        "error": "错误",
        "english": "英语",
        "simplified-chinese": "简体中文",
        "traditional-chinese": "繁体中文",
        "japanese": "日语",
        "korean": "韩语",
        "bottom-center": "底部居中",
        "bottom-left": "左下角",
        "bottom-right": "右下角"
    })
    property FileView preferenceFile

    function format(text, values) {
        let result = String(text);
        for (let i = 0; i < values.length; ++i) result = result.replace(new RegExp("\\{" + i + "\\}", "g"), String(values[i]))
        return result;
    }

    function tr(text) {
        if (text === undefined || text === null)
            return "";

        const source = String(text);
        if (root.locale !== "zh-CN")
            return source;

        if (root.zh[source] !== undefined)
            return root.zh[source];

        let match = source.match(/^Open (.+)$/);
        if (match)
            return "打开" + root.tr(match[1]);

        match = source.match(/^Configured via (.+)$/);
        if (match)
            return "已通过 " + match[1] + " 配置";

        match = source.match(/^(.*)\. Blank keeps it unchanged\. API keys are region-scoped; Voice Input never probes another region or migrates a key automatically\.$/);
        if (match)
            return root.tr(match[1]) + "。留空将保持不变。API key 受区域范围约束；语音输入绝不会探测其他区域，也不会自动迁移 key。";

        match = source.match(/^(.*)\. Blank keeps it unchanged\.$/);
        if (match)
            return root.tr(match[1]) + "。留空将保持不变。";

        match = source.match(/^(.*)\. Blank uses the stored credential\.$/);
        if (match)
            return root.tr(match[1]) + "。留空将使用已存储的凭据。";

        match = source.match(/^Settings backend exited \(code (.+)\)\.$/);
        if (match)
            return "设置后端已退出（代码 " + match[1] + "）。";

        match = source.match(/^Unsupported settings protocol version: (.+)$/);
        if (match)
            return "不支持的设置协议版本：" + match[1];

        match = source.match(/^Enter a whole number(?: of at least (.+))?\.$/);
        if (match)
            return match[1] === undefined ? "请输入整数。" : "请输入不小于 " + match[1] + " 的整数。";

        match = source.match(/^Enter a finite number(?: of at least (.+))?\.$/);
        if (match)
            return match[1] === undefined ? "请输入有限数值。" : "请输入不小于 " + match[1] + " 的有限数值。";

        match = source.match(/^Vocabulary line (.+) must be a JSON object with term and weight\.$/);
        if (match)
            return "动态词汇表第 " + match[1] + " 行必须是包含 term 和 weight 的 JSON 对象。";

        match = source.match(/^entry (.+) must not contain control characters$/);
        if (match)
            return "第 " + match[1] + " 个词条不得包含控制字符";

        match = source.match(/^entry (.+) must not be empty$/);
        if (match)
            return "第 " + match[1] + " 个词条不得为空";

        match = source.match(/^entry (.+) must contain at most (.+)$/);
        if (match)
            return "第 " + match[1] + " 个词条最多只能包含 " + match[2];

        match = source.match(/^entry (.+) weight must be between 1 and 5 or exactly 50$/);
        if (match)
            return "第 " + match[1] + " 个词条的权重必须介于 1 到 5 之间，或恰好为 50";

        match = source.match(/^entry (.+) duplicates an earlier entry after trimming$/);
        if (match)
            return "第 " + match[1] + " 个词条去除首尾空白后与前面的词条重复";

        match = source.match(/^must contain at most (.+) configured term bytes$/);
        if (match)
            return "配置的词条最多只能包含 " + match[1] + " 字节";

        match = source.match(/^must contain at most (.+) entries with weight 50$/);
        if (match)
            return "权重为 50 的词条最多只能有 " + match[1] + " 个";

        match = source.match(/^must contain at most (.+) entries$/);
        if (match)
            return "最多只能包含 " + match[1] + " 个词条";

        match = source.match(/^must be at most (.+) bytes$/);
        if (match)
            return "最多只能包含 " + match[1] + " 字节";

        match = source.match(/^must be between (.+) and (.+)$/);
        if (match)
            return "必须介于 " + match[1] + " 和 " + match[2] + " 之间";

        match = source.match(/^must use (.+)$/);
        if (match)
            return "必须使用 " + match[1];

        match = source.match(/^Service state: (.+)$/);
        if (match)
            return "服务状态：" + root.tr(match[1]);

        match = source.match(/^Phase: (.+)$/);
        if (match)
            return "阶段：" + root.tr(match[1]);

        match = source.match(/^Language: (.+)$/);
        if (match)
            return "语言：" + root.tr(match[1]);

        match = source.match(/^Updated: (.+)$/);
        if (match)
            return "更新时间：" + match[1];

        match = source.match(/^Model: (.+)$/);
        if (match)
            return "模型：" + match[1];

        match = source.match(/^Position: (.+)$/);
        if (match)
            return "位置：" + root.tr(match[1].replace(/ /g, "-"));

        return source;
    }

    function setLocale(nextLocale) {
        if (nextLocale !== "en" && nextLocale !== "zh-CN")
            return ;

        explicitChoice = true;
        locale = nextLocale;
        preferenceAdapter.locale = nextLocale;
        try {
            preferenceFile.writeAdapter();
        } catch (error) {
        }
    }

    function loadPreference() {
        if (explicitChoice)
            return ;

        try {
            if (preferenceAdapter.locale === "en" || preferenceAdapter.locale === "zh-CN")
                locale = preferenceAdapter.locale;

        } catch (error) {
        }
    }

    preferenceFile: FileView {
        path: root.preferencePath
        atomicWrites: true
        blockAllReads: true
        printErrors: false
        onLoaded: root.loadPreference()

        JsonAdapter {
            id: preferenceAdapter

            property string locale: ""
        }

    }

}
