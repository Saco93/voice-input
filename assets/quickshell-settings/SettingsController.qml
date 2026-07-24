import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    id: root

    readonly property int protocolVersion: 1
    readonly property string backendBinary: Quickshell.env("VOICE_INPUT_BIN")
    readonly property bool backendConfigured: backendBinary.length > 0
    property int nextRequestId: 1
    property var pending: ({})
    property var loadedConfig: ({})
    property var draft: ({})
    property var credentials: ({})
    property var fieldErrors: ({})
    property string globalError: ""
    property string statusMessage: ""
    property bool loading: false
    property bool saving: false
    property bool testing: false
    property var revision: null

    // Secrets live only in these transient properties. They are never copied into
    // draft config, logged, or placed in process arguments.
    property string alibabaCredential: ""
    property string openrouterCredential: ""

    readonly property bool busy: loading || saving || testing
    readonly property bool dirty: JSON.stringify(draft) !== JSON.stringify(loadedConfig)
        || alibabaCredential.length > 0 || openrouterCredential.length > 0

    signal loaded()
    signal saved()

    function defaults() {
        return {
            "state_file": "auto",
            "hotkey": {"accelerator": "SUPER CTRL, X", "mode": "hold"},
            "audio": {
                "device": "default", "sample_rate": 16000,
                "max_duration_secs": 90, "partial_interval_ms": 1500,
                "pre_roll_enabled": false, "pre_roll_ms": 500
            },
            "asr": {
                "provider": "local-cli", "backend_command": "/usr/bin/voxtype",
                "engine": "sensevoice", "model": "",
                "language": "simplified-chinese", "connect_timeout_ms": 5000,
                "finalize_timeout_ms": 8000, "fallback_to_local": true,
                "alibaba": {
                    "endpoint": "wss://dashscope.aliyuncs.com/api-ws/v1/realtime",
                    "model": "qwen3-asr-flash-realtime-2026-02-10",
                    "turn_mode": "server-vad", "vad_threshold": 0.2,
                    "silence_duration_ms": 400, "final_pass_enabled": false,
                    "final_pass_base_url": "",
                    "final_pass_model": "qwen3-asr-flash-2026-02-10",
                    "final_pass_timeout_ms": 20000,
                    "final_pass_enable_itn": false
                }
            },
            "output": {
                "mode": "type", "fallback_to_clipboard": true,
                "type_delay_ms": 0, "pre_type_delay_ms": 140,
                "paste_keys": "shift+Insert", "prefer_paste_for_xwayland": true,
                "xwayland_paste_keys": "shift+Insert"
            },
            "ime": {"manage_fcitx5": true, "force_ascii_before_output": true},
            "llm": {
                "enabled": false, "api_base_url": "https://api.openai.com/v1",
                "model": "", "timeout_ms": 5000, "provider_sort": "",
                "agent_context_enabled": false, "agent_context_max_chars": 6000
            },
            "hud": {
                "enabled": true, "margin_bottom": 72, "height": 56,
                "position": "bottom-center", "offset_x": 0, "offset_y": 0,
                "nudge_step": 24
            }
        };
    }

    function clone(value) {
        return JSON.parse(JSON.stringify(value));
    }

    function withoutPlaintextSecrets(config) {
        const safe = clone(config);
        if (safe.asr && safe.asr.alibaba)
            delete safe.asr.alibaba.api_key;
        if (safe.llm)
            delete safe.llm.api_key;
        return safe;
    }

    function merge(base, supplied) {
        if (!supplied || typeof supplied !== "object" || Array.isArray(supplied))
            return supplied === undefined ? clone(base) : supplied;
        const result = clone(base);
        for (const key in supplied) {
            if (result[key] && typeof result[key] === "object"
                    && !Array.isArray(result[key])
                    && typeof supplied[key] === "object")
                result[key] = merge(result[key], supplied[key]);
            else
                result[key] = clone(supplied[key]);
        }
        return result;
    }

    function value(path, fallback) {
        let current = draft;
        const parts = path.split(".");
        for (let i = 0; i < parts.length; ++i) {
            if (current === undefined || current === null
                    || current[parts[i]] === undefined)
                return fallback;
            current = current[parts[i]];
        }
        return current;
    }

    function setValue(path, value) {
        const next = clone(draft);
        const parts = path.split(".");
        let current = next;
        for (let i = 0; i < parts.length - 1; ++i) {
            if (!current[parts[i]])
                current[parts[i]] = {};
            current = current[parts[i]];
        }
        current[parts[parts.length - 1]] = value;
        draft = next;
        clearFieldError(path);
        statusMessage = "";
    }

    function errorFor(path) {
        const value = fieldErrors[path];
        if (Array.isArray(value))
            return value.join("; ");
        return value ? String(value) : "";
    }

    function clearFieldError(path) {
        if (fieldErrors[path] === undefined)
            return;
        const next = clone(fieldErrors);
        delete next[path];
        fieldErrors = next;
    }

    function clearMessages() {
        globalError = "";
        fieldErrors = ({});
        statusMessage = "";
    }

    function credentialLabel(id) {
        const metadata = credentials[id] || {};
        if (!metadata.configured)
            return "Not configured";
        return metadata.source ? "Configured via " + metadata.source : "Configured";
    }

    function send(method, params) {
        if (!backendProcess.running) {
            globalError = backendConfigured
                ? "Settings backend is not running."
                : "VOICE_INPUT_BIN is not set; cannot start the settings backend.";
            return -1;
        }
        const id = nextRequestId++;
        const nextPending = clone(pending);
        nextPending[String(id)] = {"method": method, "params": params};
        pending = nextPending;

        // Versioned NDJSON is the only backend channel. JSON and the trailing
        // newline are written to stdin; no shell is involved.
        backendProcess.write(JSON.stringify({
            "version": protocolVersion,
            "id": id,
            "method": method,
            "params": params
        }) + "\n");
        return id;
    }

    function reload() {
        if (busy)
            return;
        clearMessages();
        loading = true;
        if (send("settings.get", {}) < 0)
            loading = false;
    }

    function asNumber(config, path, integer, minimum) {
        let current = config;
        const parts = path.split(".");
        for (let i = 0; i < parts.length - 1; ++i)
            current = current[parts[i]];
        const key = parts[parts.length - 1];
        const numeric = Number(current[key]);
        if (!Number.isFinite(numeric) || (integer && !Number.isInteger(numeric))
                || (minimum !== null && numeric < minimum)) {
            const next = clone(fieldErrors);
            next[path] = integer
                ? "Enter a whole number" + (minimum !== null ? " of at least " + minimum : "") + "."
                : "Enter a finite number" + (minimum !== null ? " of at least " + minimum : "") + ".";
            fieldErrors = next;
            return false;
        }
        current[key] = numeric;
        return true;
    }

    function normalizedDraft() {
        fieldErrors = ({});
        const config = withoutPlaintextSecrets(draft);
        const unsignedIntegers = [
            ["audio.sample_rate", 1], ["audio.max_duration_secs", 0],
            ["audio.partial_interval_ms", 0], ["audio.pre_roll_ms", 0],
            ["asr.connect_timeout_ms", 0], ["asr.finalize_timeout_ms", 0],
            ["asr.alibaba.silence_duration_ms", 0],
            ["asr.alibaba.final_pass_timeout_ms", 0],
            ["output.type_delay_ms", 0], ["output.pre_type_delay_ms", 0],
            ["llm.timeout_ms", 0], ["llm.agent_context_max_chars", 0]
        ];
        const signedIntegers = [
            "hud.margin_bottom", "hud.height", "hud.offset_x", "hud.offset_y",
            "hud.nudge_step"
        ];
        let valid = true;
        for (let i = 0; i < unsignedIntegers.length; ++i)
            valid = asNumber(config, unsignedIntegers[i][0], true,
                             unsignedIntegers[i][1]) && valid;
        for (let i = 0; i < signedIntegers.length; ++i)
            valid = asNumber(config, signedIntegers[i], true, null) && valid;
        valid = asNumber(config, "asr.alibaba.vad_threshold", false, null) && valid;
        if (!valid) {
            globalError = "Fix the highlighted numeric fields before continuing.";
            return null;
        }
        return config;
    }

    function save() {
        if (busy)
            return;
        clearMessages();
        const config = normalizedDraft();
        if (!config)
            return;
        saving = true;
        const enteredAlibaba = alibabaCredential;
        const enteredOpenrouter = openrouterCredential;
        const id = send("settings.save", {
            "revision": revision,
            "config": config,
            "credentials": {
                "alibaba-api-key": enteredAlibaba.length > 0
                    ? {"action": "replace", "value": enteredAlibaba}
                    : {"action": "keep"},
                "openrouter-api-key": enteredOpenrouter.length > 0
                    ? {"action": "replace", "value": enteredOpenrouter}
                    : {"action": "keep"}
            },
            "restart": true
        });
        // Clear password fields immediately after the complete request has been
        // handed to Process.write(). Secrets remain only in the serialized stdin
        // buffer owned by Quickshell and the backend.
        if (id >= 0) {
            alibabaCredential = "";
            openrouterCredential = "";
        } else {
            saving = false;
        }
    }

    function testLlm() {
        if (busy)
            return;
        clearMessages();
        const config = normalizedDraft();
        if (!config)
            return;
        testing = true;
        const entered = openrouterCredential;
        const id = send("llm.test", {
            "llm": config.llm,
            "credential": entered.length > 0
                ? {"source": "entered", "value": entered}
                : {"source": "store"}
        });
        // As with Save, never retain an entered secret after it has been sent.
        if (id >= 0)
            openrouterCredential = "";
        else
            testing = false;
    }

    function applyBackendFields(fields) {
        if (!fields) {
            fieldErrors = ({});
            return;
        }
        if (Array.isArray(fields)) {
            const mapped = {};
            for (let i = 0; i < fields.length; ++i) {
                const item = fields[i];
                if (item && item.field)
                    mapped[item.field] = item.message || "Invalid value.";
            }
            fieldErrors = mapped;
        } else {
            fieldErrors = clone(fields);
        }
    }

    function consumeLine(line) {
        if (!line || line.trim().length === 0)
            return;
        let response;
        try {
            response = JSON.parse(line);
        } catch (error) {
            globalError = "The settings backend returned malformed JSON.";
            return;
        }
        if (response.version !== protocolVersion) {
            globalError = "Unsupported settings protocol version: " + response.version;
            return;
        }
        const key = String(response.id);
        const request = pending[key];
        if (!request)
            return;
        const nextPending = clone(pending);
        delete nextPending[key];
        pending = nextPending;

        const method = request.method;
        if (method === "settings.get")
            loading = false;
        else if (method === "settings.save")
            saving = false;
        else if (method === "llm.test")
            testing = false;

        if (!response.ok) {
            const error = response.error || {};
            globalError = error.message || "The settings backend rejected the request.";
            applyBackendFields(error.fields);
            return;
        }

        const result = response.result || {};
        if (method === "settings.get") {
            const config = withoutPlaintextSecrets(merge(defaults(), result.config || {}));
            loadedConfig = clone(config);
            draft = clone(config);
            revision = result.revision === undefined ? null : result.revision;
            credentials = clone(result.credentials || {});
            statusMessage = "Settings loaded.";
            loaded();
        } else if (method === "settings.save") {
            const config = withoutPlaintextSecrets(merge(defaults(), result.config || request.params.config));
            loadedConfig = clone(config);
            draft = clone(config);
            if (result.revision !== undefined)
                revision = result.revision;
            if (result.credentials)
                credentials = clone(result.credentials);
            if (result.partial) {
                const partialFields = {};
                const credentialErrors = result.credential_errors || {};
                for (const credentialId in credentialErrors)
                    partialFields["credentials." + credentialId] = credentialErrors[credentialId];
                fieldErrors = partialFields;
                globalError = result.message || "Settings were saved with additional errors.";
                statusMessage = "";
            } else {
                statusMessage = result.message || "Saved and restarted Voice Input.";
            }
            saved();
        } else if (method === "llm.test") {
            statusMessage = result.message || "LLM connection test succeeded.";
        }
    }

    property Process backendProcess: Process {
        command: root.backendConfigured
            ? [root.backendBinary, "settings-backend", "--stdio"] : []
        stdinEnabled: true
        running: root.backendConfigured

        // stdout is exclusively versioned, newline-delimited JSON. SplitParser
        // preserves message boundaries even when reads are partial or coalesced.
        stdout: SplitParser {
            splitMarker: "\n"
            onRead: data => root.consumeLine(data)
        }
        stderr: SplitParser {
            splitMarker: "\n"
            onRead: data => {
                // Do not surface or log arbitrary stderr because a backend must
                // never accidentally expose credential material through this UI.
                if (data.trim().length > 0 && root.globalError.length === 0)
                    root.globalError = "The settings backend reported an internal error.";
            }
        }
        onStarted: root.reload()
        onExited: (exitCode, exitStatus) => {
            root.loading = false;
            root.saving = false;
            root.testing = false;
            if (root.globalError.length === 0)
                root.globalError = "Settings backend exited (code " + exitCode + ").";
        }
    }

    Component.onCompleted: {
        loadedConfig = defaults();
        draft = clone(loadedConfig);
        if (!backendConfigured)
            globalError = "Set VOICE_INPUT_BIN to the Voice Input executable path.";
    }
}
