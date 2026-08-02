import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    id: root

    readonly property int protocolVersion: 1
    readonly property int requestTimeoutMs: 30000
    readonly property string backendBinary: Quickshell.env("VOICE_INPUT_BIN")
    readonly property bool backendConfigured: backendBinary.length > 0
    property int nextRequestId: 1
    property var pending: ({
    })
    property var loadedConfig: ({
    })
    property var draft: ({
    })
    property var credentials: ({
    })
    property var fieldErrors: ({
    })
    property string globalError: ""
    property string statusMessage: ""
    property bool loading: false
    property bool saving: false
    property bool testing: false
    property bool runtimeLoading: false
    property bool runtimeRefreshPending: false
    property var runtimeStatus: ({
    })
    property string runtimeStatusState: "unknown"
    property string runtimeStatusMessage: ""
    property var revision: null
    // Secrets live only in these transient properties. They are never copied into
    // draft config, logged, or placed in process arguments.
    property string alibabaCredential: ""
    property string openrouterCredential: ""
    property string audio3VocabularyText: ""
    readonly property bool busy: loading || saving || testing
    readonly property bool audio3VocabularyDirty: audio3VocabularyText !== formatAudio3Vocabulary(loadedConfig)
    readonly property bool dirty: JSON.stringify(draft) !== JSON.stringify(loadedConfig) || audio3VocabularyDirty || alibabaCredential.length > 0 || openrouterCredential.length > 0
    property Process backendProcess
    property Timer runtimePollTimer
    property Timer requestTimeoutTimer
    property Timer backendRestartTimer
    property bool backendRestarting: false

    signal loaded()
    signal saved()
    signal routeRequested(string page, bool advanced)

    function defaults() {
        return {
            "state_file": "auto",
            "hotkey": {
                "accelerator": ", F9",
                "mode": "toggle"
            },
            "audio": {
                "device": "default",
                "sample_rate": 16000,
                "max_duration_secs": 300,
                "partial_interval_ms": 1500,
                "pre_roll_enabled": false,
                "pre_roll_ms": 500
            },
            "asr": {
                "provider": "local-cli",
                "backend_command": "/usr/bin/voxtype",
                "engine": "sensevoice",
                "model": "",
                "language": "simplified-chinese",
                "connect_timeout_ms": 5000,
                "finalize_timeout_ms": 8000,
                "fallback_to_local": true,
                "alibaba": {
                    "endpoint": "wss://dashscope.aliyuncs.com/api-ws/v1/realtime",
                    "model": "qwen3-asr-flash-realtime-2026-02-10",
                    "turn_mode": "server-vad",
                    "vad_threshold": 0.2,
                    "silence_duration_ms": 400,
                    "final_pass_enabled": false,
                    "final_pass_base_url": "",
                    "final_pass_model": "qwen3-asr-flash-2026-02-10",
                    "final_pass_timeout_ms": 20000,
                    "final_pass_enable_itn": false
                },
                "alibaba_audio3": {
                    "experimental_enabled": false,
                    "endpoint": "wss://dashscope.aliyuncs.com/api-ws/v1/inference",
                    "model": "qwen-audio-3.0-asr-flash-streaming",
                    "language_hints_enabled": false,
                    "heartbeat_enabled": false,
                    "vocabulary": [],
                    "native_endpoint": "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation",
                    "native_model": "qwen-audio-3.0-asr-flash",
                    "native_final_pass_enabled": false,
                    "native_timeout_ms": 20000
                }
            },
            "output": {
                "mode": "paste",
                "fallback_to_clipboard": true,
                "type_delay_ms": 0,
                "pre_type_delay_ms": 140,
                "paste_keys": "shift+Insert",
                "prefer_paste_for_xwayland": true,
                "xwayland_paste_keys": "shift+Insert"
            },
            "ime": {
                "manage_fcitx5": true,
                "force_ascii_before_output": true
            },
            "llm": {
                "enabled": false,
                "api_base_url": "https://api.openai.com/v1",
                "model": "",
                "timeout_ms": 15000,
                "provider_sort": "",
                "agent_context_enabled": false,
                "agent_context_max_chars": 6000
            },
            "hud": {
                "enabled": true,
                "margin_bottom": 72,
                "height": 56,
                "position": "bottom-center",
                "offset_x": 0,
                "offset_y": 0,
                "nudge_step": 24
            }
        };
    }

    function clone(value) {
        return JSON.parse(JSON.stringify(value));
    }

    function formatAudio3Vocabulary(config) {
        const audio3 = config && config.asr ? config.asr.alibaba_audio3 : null;
        const entries = audio3 && Array.isArray(audio3.vocabulary) ? audio3.vocabulary : [];
        const lines = [];
        for (let i = 0; i < entries.length; ++i) lines.push(JSON.stringify({
            "term": entries[i].term,
            "weight": entries[i].weight
        }))
        return lines.join("\n");
    }

    function setAudio3VocabularyText(value) {
        audio3VocabularyText = value;
        clearFieldError("asr.alibaba_audio3.vocabulary");
        statusMessage = "";
    }

    function parseAudio3Vocabulary() {
        const entries = [];
        const lines = audio3VocabularyText.split(/\r?\n/);
        for (let i = 0; i < lines.length; ++i) {
            if (lines[i].trim().length === 0)
                continue;

            let entry;
            try {
                entry = JSON.parse(lines[i]);
            } catch (error) {
                const next = clone(fieldErrors);
                next["asr.alibaba_audio3.vocabulary"] = "Vocabulary line " + (i + 1) + " must be a JSON object with term and weight.";
                fieldErrors = next;
                return null;
            }
            const keys = entry && typeof entry === "object" && !Array.isArray(entry) ? Object.keys(entry) : [];
            if (keys.length !== 2 || !Object.prototype.hasOwnProperty.call(entry, "term") || !Object.prototype.hasOwnProperty.call(entry, "weight") || typeof entry.term !== "string" || typeof entry.weight !== "number" || !Number.isInteger(entry.weight)) {
                const next = clone(fieldErrors);
                next["asr.alibaba_audio3.vocabulary"] = "Vocabulary line " + (i + 1) + " must be a JSON object with term and weight.";
                fieldErrors = next;
                return null;
            }
            entries.push({
                "term": entry.term.trim(),
                "weight": entry.weight
            });
        }
        return entries;
    }

    function withoutPlaintextSecrets(config) {
        const safe = clone(config);
        if (safe.asr && safe.asr.alibaba)
            delete safe.asr.alibaba.api_key;

        if (safe.asr && safe.asr.alibaba_audio3)
            delete safe.asr.alibaba_audio3.api_key;

        if (safe.llm)
            delete safe.llm.api_key;

        return safe;
    }

    function merge(base, supplied) {
        if (!supplied || typeof supplied !== "object" || Array.isArray(supplied))
            return supplied === undefined ? clone(base) : supplied;

        const result = clone(base);
        for (const key in supplied) {
            if (result[key] && typeof result[key] === "object" && !Array.isArray(result[key]) && typeof supplied[key] === "object")
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
            if (current === undefined || current === null || current[parts[i]] === undefined)
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
                current[parts[i]] = {
            };

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
            return ;

        const next = clone(fieldErrors);
        delete next[path];
        fieldErrors = next;
    }

    function clearMessages() {
        globalError = "";
        fieldErrors = ({
        });
        statusMessage = "";
    }

    function discard() {
        if (busy)
            return ;

        draft = clone(loadedConfig);
        audio3VocabularyText = formatAudio3Vocabulary(loadedConfig);
        alibabaCredential = "";
        openrouterCredential = "";
        clearMessages();
    }

    function pageForField(path) {
        if (path === "state_file" || path.indexOf("hotkey.") === 0)
            return "Hotkey & state";

        if (path.indexOf("audio.") === 0 || path.indexOf("asr.") === 0 || path === "credentials.alibaba-api-key")
            return "Speech";

        if (path.indexOf("llm.") === 0 || path === "credentials.openrouter-api-key")
            return "Refinement";

        if (path.indexOf("output.") === 0 || path.indexOf("ime.") === 0)
            return "Output";

        if (path.indexOf("hud.") === 0)
            return "Appearance";

        return "Hotkey & state";
    }

    function fieldNeedsAdvanced(path) {
        const audio3Advanced = path.indexOf("asr.alibaba_audio3.") === 0 && path !== "asr.alibaba_audio3.experimental_enabled" && path !== "asr.alibaba_audio3.language_hints_enabled" && path !== "asr.alibaba_audio3.heartbeat_enabled" && path !== "asr.alibaba_audio3.vocabulary" && path !== "asr.alibaba_audio3.native_final_pass_enabled";
        return path === "audio.sample_rate" || path === "audio.partial_interval_ms" || path === "audio.pre_roll_ms" || path === "asr.connect_timeout_ms" || path === "asr.finalize_timeout_ms" || path.indexOf("asr.alibaba.endpoint") === 0 || path.indexOf("asr.alibaba.vad_") === 0 || path.indexOf("asr.alibaba.silence_") === 0 || path.indexOf("asr.alibaba.final_pass_") === 0 || audio3Advanced || path === "llm.api_base_url" || path === "llm.timeout_ms" || path === "llm.provider_sort" || path === "llm.agent_context_max_chars" || path === "output.type_delay_ms" || path === "output.pre_type_delay_ms" || path === "output.paste_keys" || path === "output.prefer_paste_for_xwayland" || path === "output.xwayland_paste_keys" || path === "hud.margin_bottom" || path === "hud.height" || path === "hud.offset_x" || path === "hud.offset_y" || path === "hud.nudge_step";
    }

    function errorCountForPage(page) {
        let count = 0;
        for (const path in fieldErrors) {
            if (pageForField(path) === page)
                count += 1;

        }
        return count;
    }

    function hasErrorPrefix(prefix) {
        for (const path in fieldErrors) {
            if (path.indexOf(prefix) === 0)
                return true;

        }
        return false;
    }

    function pageHasAdvancedError(page) {
        for (const path in fieldErrors) {
            if (pageForField(path) === page && fieldNeedsAdvanced(path))
                return true;

        }
        return false;
    }

    function routeFirstError() {
        const paths = Object.keys(fieldErrors);
        if (paths.length === 0)
            return ;

        const path = paths[0];
        routeRequested(pageForField(path), fieldNeedsAdvanced(path));
    }

    function credentialLabel(id) {
        const metadata = credentials[id] || {
        };
        if (!metadata.configured)
            return "Not configured";

        return metadata.source ? "Configured via " + metadata.source : "Configured";
    }

    function send(method, params, quiet) {
        if (!backendProcess.running) {
            if (!quiet)
                globalError = backendConfigured ? "Settings backend is not running." : "VOICE_INPUT_BIN is not set; cannot start the settings backend.";

            return -1;
        }
        const id = nextRequestId++;
        const nextPending = clone(pending);
        const requestMetadata = {
            "method": method,
            "quiet": quiet === true,
            "deadlineMs": Date.now() + requestTimeoutMs
        };
        // Pending request metadata must never retain credential request bodies.
        // Save needs only the already-sanitized config as a response fallback.
        if (method === "settings.save")
            requestMetadata.configFallback = clone(params.config);

        nextPending[String(id)] = requestMetadata;
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
            return ;

        clearMessages();
        loading = true;
        if (send("settings.get", {
        }, false) < 0)
            loading = false;

    }

    function refreshRuntimeStatus() {
        if (runtimeLoading) {
            runtimeRefreshPending = true;
            return ;
        }
        if (!backendProcess.running) {
            runtimeStatusState = "unavailable";
            runtimeStatusMessage = "Runtime status is unavailable.";
            return ;
        }
        runtimeLoading = true;
        if (send("runtime.get", {
        }, true) < 0) {
            runtimeLoading = false;
            runtimeStatusState = "unavailable";
            runtimeStatusMessage = "Runtime status is unavailable.";
        }
    }

    function asNumber(config, path, integer, minimum) {
        let current = config;
        const parts = path.split(".");
        for (let i = 0; i < parts.length - 1; ++i) current = current[parts[i]]
        const key = parts[parts.length - 1];
        const numeric = Number(current[key]);
        if (!Number.isFinite(numeric) || (integer && !Number.isInteger(numeric)) || (minimum !== null && numeric < minimum)) {
            const next = clone(fieldErrors);
            next[path] = integer ? "Enter a whole number" + (minimum !== null ? " of at least " + minimum : "") + "." : "Enter a finite number" + (minimum !== null ? " of at least " + minimum : "") + ".";
            fieldErrors = next;
            return false;
        }
        current[key] = numeric;
        return true;
    }

    function normalizedDraft() {
        fieldErrors = ({
        });
        const config = withoutPlaintextSecrets(draft);
        const unsignedIntegers = [["audio.sample_rate", 1], ["audio.max_duration_secs", 0], ["audio.partial_interval_ms", 0], ["audio.pre_roll_ms", 0], ["asr.connect_timeout_ms", 0], ["asr.finalize_timeout_ms", 0], ["asr.alibaba.silence_duration_ms", 0], ["asr.alibaba.final_pass_timeout_ms", 0], ["asr.alibaba_audio3.native_timeout_ms", 0], ["output.type_delay_ms", 0], ["output.pre_type_delay_ms", 0], ["llm.timeout_ms", 0], ["llm.agent_context_max_chars", 0]];
        const signedIntegers = ["hud.margin_bottom", "hud.height", "hud.offset_x", "hud.offset_y", "hud.nudge_step"];
        let valid = true;
        for (let i = 0; i < unsignedIntegers.length; ++i) valid = asNumber(config, unsignedIntegers[i][0], true, unsignedIntegers[i][1]) && valid
        for (let i = 0; i < signedIntegers.length; ++i) valid = asNumber(config, signedIntegers[i], true, null) && valid
        valid = asNumber(config, "asr.alibaba.vad_threshold", false, null) && valid;
        const vocabulary = parseAudio3Vocabulary();
        if (vocabulary === null)
            valid = false;
        else
            config.asr.alibaba_audio3.vocabulary = vocabulary;
        if (!valid) {
            globalError = vocabulary === null ? "Fix the highlighted fields before continuing." : "Fix the highlighted numeric fields before continuing.";
            routeFirstError();
            return null;
        }
        return config;
    }

    function save() {
        if (busy)
            return ;

        clearMessages();
        const config = normalizedDraft();
        if (!config)
            return ;

        saving = true;
        const enteredAlibaba = alibabaCredential;
        const enteredOpenrouter = openrouterCredential;
        const id = send("settings.save", {
            "revision": revision,
            "config": config,
            "credentials": {
                "alibaba-api-key": enteredAlibaba.length > 0 ? {
                    "action": "replace",
                    "value": enteredAlibaba
                } : {
                    "action": "keep"
                },
                "openrouter-api-key": enteredOpenrouter.length > 0 ? {
                    "action": "replace",
                    "value": enteredOpenrouter
                } : {
                    "action": "keep"
                }
            },
            "restart": true
        }, false);
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
            return ;

        clearMessages();
        const config = normalizedDraft();
        if (!config)
            return ;

        testing = true;
        const entered = openrouterCredential;
        const id = send("llm.test", {
            "llm": config.llm,
            "credential": entered.length > 0 ? {
                "source": "entered",
                "value": entered
            } : {
                "source": "store"
            }
        }, false);
        // As with Save, never retain an entered secret after it has been sent.
        if (id >= 0)
            openrouterCredential = "";
        else
            testing = false;
    }

    function applyBackendFields(fields) {
        if (!fields) {
            fieldErrors = ({
            });
            return ;
        }
        if (Array.isArray(fields)) {
            const mapped = {
            };
            for (let i = 0; i < fields.length; ++i) {
                const item = fields[i];
                if (item && item.field)
                    mapped[item.field] = item.message || "Invalid value.";

            }
            fieldErrors = mapped;
        } else {
            fieldErrors = clone(fields);
        }
        routeFirstError();
    }

    function failProtocol(message) {
        pending = ({
        });
        loading = false;
        saving = false;
        testing = false;
        runtimeLoading = false;
        runtimeRefreshPending = false;
        runtimeStatusState = "unavailable";
        runtimeStatusMessage = "Runtime status is unavailable.";
        if (message && message.length > 0)
            globalError = message;

    }

    function restartBackend() {
        if (!backendConfigured || backendRestarting)
            return ;

        backendRestarting = true;
        if (backendProcess.running)
            backendProcess.running = false;
        else
            backendRestartTimer.restart();
    }

    function expireRequests() {
        const now = Date.now();
        let expired = false;
        let userFacing = false;
        for (const key in pending) {
            const request = pending[key];
            if (request.deadlineMs <= now) {
                expired = true;
                userFacing = userFacing || !request.quiet;
            }
        }
        if (!expired)
            return ;

        failProtocol(userFacing ? "The settings backend did not respond in time and is being restarted. The operation may not have completed." : "");
        restartBackend();
    }

    function consumeLine(line) {
        if (!line || line.trim().length === 0)
            return ;

        let response;
        try {
            response = JSON.parse(line);
        } catch (error) {
            failProtocol("The settings backend returned malformed JSON.");
            return ;
        }
        if (!response || typeof response !== "object" || Array.isArray(response)) {
            failProtocol("The settings backend returned an invalid response.");
            return ;
        }
        if (response.version !== protocolVersion) {
            failProtocol("Unsupported settings protocol version: " + response.version);
            return ;
        }
        const key = String(response.id);
        const request = pending[key];
        if (!request)
            return ;

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
        else if (method === "runtime.get")
            runtimeLoading = false;
        if (!response.ok) {
            const error = response.error || {
            };
            if (method === "runtime.get") {
                runtimeStatusState = "unavailable";
                runtimeStatusMessage = error.code === "method_not_found" ? "Runtime status is unavailable." : "Runtime status is temporarily unavailable.";
                if (runtimeRefreshPending) {
                    runtimeRefreshPending = false;
                    refreshRuntimeStatus();
                }
                return ;
            }
            globalError = error.message || "The settings backend rejected the request.";
            applyBackendFields(error.fields);
            return ;
        }
        const result = response.result || {
        };
        if (method === "settings.get") {
            const config = withoutPlaintextSecrets(merge(defaults(), result.config || {
            }));
            loadedConfig = clone(config);
            draft = clone(config);
            audio3VocabularyText = formatAudio3Vocabulary(config);
            revision = result.revision === undefined ? null : result.revision;
            credentials = clone(result.credentials || {
            });
            statusMessage = "Settings loaded.";
            loaded();
            refreshRuntimeStatus();
        } else if (method === "settings.save") {
            const config = withoutPlaintextSecrets(merge(defaults(), result.config || request.configFallback));
            loadedConfig = clone(config);
            draft = clone(config);
            audio3VocabularyText = formatAudio3Vocabulary(config);
            if (result.revision !== undefined)
                revision = result.revision;

            if (result.credentials)
                credentials = clone(result.credentials);

            if (result.partial) {
                const partialFields = {
                };
                const credentialErrors = result.credential_errors || {
                };
                for (const credentialId in credentialErrors) partialFields["credentials." + credentialId] = credentialErrors[credentialId]
                fieldErrors = partialFields;
                routeFirstError();
                globalError = result.message || "Settings were saved with additional errors.";
                statusMessage = "";
            } else {
                statusMessage = result.message || "Saved and restarted Voice Input.";
            }
            saved();
            refreshRuntimeStatus();
        } else if (method === "llm.test") {
            statusMessage = result.message || "LLM connection test succeeded.";
        } else if (method === "runtime.get") {
            runtimeStatus = clone(result);
            runtimeStatusState = "available";
            runtimeStatusMessage = "";
            if (runtimeRefreshPending) {
                runtimeRefreshPending = false;
                refreshRuntimeStatus();
            }
        }
    }

    Component.onCompleted: {
        loadedConfig = defaults();
        draft = clone(loadedConfig);
        audio3VocabularyText = formatAudio3Vocabulary(loadedConfig);
        if (!backendConfigured)
            globalError = "Set VOICE_INPUT_BIN to the Voice Input executable path.";

    }

    backendProcess: Process {
        command: root.backendConfigured ? [root.backendBinary, "settings-backend", "--stdio"] : []
        stdinEnabled: true
        running: root.backendConfigured
        onStarted: {
            root.backendRestarting = false;
            root.reload();
        }
        onExited: (exitCode, exitStatus) => {
            root.pending = ({
            });
            root.loading = false;
            root.saving = false;
            root.testing = false;
            root.runtimeLoading = false;
            root.runtimeRefreshPending = false;
            root.runtimeStatusState = "unavailable";
            root.runtimeStatusMessage = "Runtime status is unavailable.";
            if (root.backendRestarting)
                root.backendRestartTimer.restart();
            else if (root.globalError.length === 0)
                root.globalError = "Settings backend exited (code " + exitCode + ").";
        }

        // stdout is exclusively versioned, newline-delimited JSON. SplitParser
        // preserves message boundaries even when reads are partial or coalesced.
        stdout: SplitParser {
            splitMarker: "\n"
            onRead: (data) => {
                return root.consumeLine(data);
            }
        }

        stderr: SplitParser {
            splitMarker: "\n"
            onRead: (data) => {
                // Do not surface or log arbitrary stderr because a backend must
                // never accidentally expose credential material through this UI.
                if (data.trim().length > 0 && root.globalError.length === 0)
                    root.globalError = "The settings backend reported an internal error.";

            }
        }

    }

    runtimePollTimer: Timer {
        interval: 5000
        repeat: true
        running: root.backendConfigured
        onTriggered: root.refreshRuntimeStatus()
    }

    requestTimeoutTimer: Timer {
        interval: 500
        repeat: true
        running: Object.keys(root.pending).length > 0
        onTriggered: root.expireRequests()
    }

    backendRestartTimer: Timer {
        interval: 250
        repeat: false
        onTriggered: {
            if (root.backendConfigured)
                root.backendProcess.running = true;
            else
                root.backendRestarting = false;
        }
    }

}
