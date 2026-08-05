# Qwen-Audio-3 Milestone 2 API Decisions

Research date: 2026-08-05

This document records provider-facing decisions for Milestone 2 before runtime implementation. It uses current official Alibaba Cloud Model Studio documentation as the source of truth. No provider request was made during this research.

## Scope

Milestone 2 is limited to:

1. Audio3 streaming VAD and sentence-boundary controls with named presets;
2. bounded timestamp parsing and timestamp-aware assembly foundations;
3. Beijing/Singapore regional and workspace endpoint routing.

It does not add reconnect/replay, `continue-task` context, application/window collection, Filetrans, diarization, or new credential-discovery behavior.

## VAD and sentence-boundary controls

All confirmed controls belong under `payload.parameters` in the initial `run-task` event.

| Field | Type | Official default | Valid range | Decision |
| --- | --- | --- | --- | --- |
| `semantic_punctuation_enabled` | Boolean | `false` | Boolean | Implement. When enabled, semantic segmentation replaces VAD segmentation. |
| `max_sentence_silence` | Integer milliseconds | `1300` | 200–6000 | Already implemented. Voice Input's existing `800` default remains the standard preset for backward compatibility. |
| `multi_threshold_mode_enabled` | Boolean | `false` | Boolean | Implement. It is effective only when semantic punctuation is disabled. |
| `speech_noise_threshold` | Float | Not documented | -1.0–1.0 | Implement as optional custom tuning. Omission preserves provider behavior because no official default is published. |
| `heartbeat` | Boolean | `false` | Boolean | Already implemented. It remains independent of segmentation controls. |

There are no documented Audio3 fields named `speech_threshold` or `noise_threshold`. Voice Input will not send or expose those inferred names.

Official interaction rules:

- `semantic_punctuation_enabled = true` disables VAD segmentation. In this mode, `max_sentence_silence` does not cause `sentence_end`, although an excessively low value may still reduce recognition quality.
- `multi_threshold_mode_enabled` takes effect only when semantic punctuation is disabled.
- Values of `speech_noise_threshold` near `-1` classify more noise as speech; values near `1` classify more speech as noise. Alibaba publishes no default.
- Official documentation does not state whether unknown, wrongly typed, or unsupported fields are rejected or ignored. Deterministic client validation will reject invalid local values; live tests must confirm accepted combinations.

### Preset decisions

The configuration will retain raw custom values and add a named preset resolver:

- `standard`: preserve the existing effective request (`800` ms, semantic punctuation disabled, multi-threshold disabled, speech/noise threshold omitted).
- `low-latency-dictation`: candidate values will shorten sentence silence while retaining VAD segmentation. Exact values remain provisional until fixed-audio pause evaluation.
- `long-form`: candidate values will enable semantic segmentation and use the official silence default as a conservative recognition setting. Exact behavior remains provisional until long-pause evaluation.
- `custom`: send the user's validated raw controls. `speech_noise_threshold` remains optional.

`standard` remains the default. A preset will not become the default based only on API documentation.

## Timestamp schema and safe use

Audio3 streaming returns timestamps without a request opt-in.

Confirmed `result-generated` fields under `payload.output.sentence`:

| Field | Shape and meaning |
| --- | --- |
| `sentence_id` | Integer sequence identifier. Normal results begin at 1 and increment; heartbeat results use 0. |
| `begin_time` | Integer sentence start in milliseconds. |
| `end_time` | Integer sentence end in milliseconds; it may be `null` for an intermediate result. |
| `sentence_end` | `false` for intermediate output and `true` for a final sentence. |
| `words[]` | Timestamped word/segment entries containing integer-millisecond `begin_time` and `end_time`, plus text and punctuation. |

The Chinese documentation describes `words[]` as character-level in one place, while its examples contain multi-character entries and the English documentation calls it word-level. Voice Input will call these entries timed units and will not promise per-character timestamps. It will not retain a second copy of their text or punctuation.

A heartbeat result has `heartbeat = true` and `sentence_id = 0`; it must be discarded before transcript or timestamp processing.

Official documentation does not guarantee:

- that every partial and final revision for one sentence keeps the same `sentence_id`;
- that partial text, times, or timed units cannot be revised;
- that a final event cannot repeat;
- correction, retraction, reconnect, replay, or idempotency semantics.

Therefore Milestone 2 will initially:

- parse optional sentence and timed-unit ranges;
- validate type, range, event-local timed-unit monotonicity, and bounded counts without dropping valid transcript text when metadata is malformed; no monotonicity assumption will be applied across partial revisions;
- retain only bounded numeric metadata and counters;
- keep untimed transcript assembly behavior unchanged;
- treat `task-finished` as task completion after preceding `result-generated` events.

Duplicate-final suppression and correction replacement remain blocked unless Alibaba publishes a stable revision-identity contract or an implementation is proven correct even when IDs and ranges are reused or revised. A finite live capture can validate parser compatibility and reveal counterexamples, but it cannot establish an undocumented identity guarantee. Reconnect and replay remain Milestone 3 and will not be implemented here.

## Regional and workspace endpoints

Both Audio3 model variants are officially available in Beijing and Singapore with unchanged model IDs.

### Canonical endpoint matrix

| API | Region | Legacy endpoint | Current workspace endpoint template |
| --- | --- | --- | --- |
| Streaming WebSocket | Beijing | `wss://dashscope.aliyuncs.com/api-ws/v1/inference` | `wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference` |
| Streaming WebSocket | Singapore | `wss://dashscope-intl.aliyuncs.com/api-ws/v1/inference` | `wss://{WorkspaceId}.ap-southeast-1.maas.aliyuncs.com/api-ws/v1/inference` |
| Native HTTP | Beijing | `https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation` | `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation` |
| Native HTTP | Singapore | `https://dashscope-intl.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation` | `https://{WorkspaceId}.ap-southeast-1.maas.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation` |

Alibaba recommends workspace-specific hosts and states that legacy hosts remain usable. Current workspace routing places Workspace ID in the hostname's leftmost label. It is not a query parameter, path component, or request-body field.

The legacy WebSocket documentation also describes an optional `X-DashScope-WorkSpace` header. The Native HTTP reference does not describe that header and instead documents workspace-specific hosts. Voice Input will use one consistent hostname-derived mechanism for both APIs rather than inventing Native header behavior.

### Configuration and migration decisions

- Add `regional` and `custom` endpoint modes.
- Add explicit `beijing` and `singapore` regions.
- In regional mode, an empty Workspace ID uses the official legacy endpoint; a configured Workspace ID derives the reviewed workspace hostname.
- Existing exact Beijing legacy endpoint pairs migrate to regional Beijing with an empty Workspace ID.
- Existing exact Singapore legacy endpoint pairs migrate to regional Singapore with an empty Workspace ID.
- Any noncanonical, mixed, proxied, loopback, query-bearing, or otherwise overridden pair migrates to custom and remains byte-for-byte preserved.
- Regional hosts are selected from constants. Region input is never concatenated into a host.
- Workspace ID has no published provider grammar or length. Voice Input treats it as opaque and applies only the RFC-compatible DNS-label constraints required to place it safely in a hostname. These constraints are transport validation, not a claim about Alibaba's business identifier format.

API keys are region-specific and workspace-scoped. A key cannot be assumed to work across regions or workspaces. Voice Input will retain one encrypted Alibaba credential, warn that changing region/workspace may require replacing it, and never probe another region automatically.

Official pages do not provide a complete per-region matrix for language hints, heartbeat, vocabulary, sentence controls, thresholds, and timestamp behavior. The shared API reference documents these controls without a regional exclusion. Each model, field combination, and scenario used in Singapore still requires its own authorized live validation; the result will not be presented as complete feature parity.

## Implementation and live-test gates

### Implement deterministically

- recognition preset/config migration and an effective-control resolver;
- exact request-envelope fields for confirmed VAD controls;
- bounded timestamp parsing and aggregate-only diagnostics;
- regional/custom endpoint migration and pure endpoint resolution;
- hostname-based workspace routing;
- safe settings controls and privacy regression tests.

### Block pending evidence

- separate speech/noise threshold fields;
- guaranteed character-level timing;
- identity-dependent timestamp duplicate suppression or correction replacement without an official contract or an algorithm that remains correct under revisions;
- inferred Workspace ID business grammar;
- automatic credential-region probing;
- claims of complete regional feature parity.

### Require authorized live validation

- low-latency and long-form preset values;
- accepted VAD field combinations and observable segmentation effects;
- observed partial/final `sentence_id` behavior, for parser validation and counterexample discovery only;
- the specific Beijing/Singapore models, fields, and scenarios exercised by the evaluation, without claiming complete regional parity;
- Singapore dynamic vocabulary;
- workspace-specific Streaming and Native routing with a credential belonging to that region/workspace.

## Official sources

- [Realtime recognition client events](https://help.aliyun.com/zh/model-studio/fun-asr-client-events)
- [Realtime recognition server events](https://help.aliyun.com/zh/model-studio/fun-asr-server-events)
- [Realtime WebSocket API](https://help.aliyun.com/zh/model-studio/fun-asr-realtime-websocket-api)
- [Realtime recognition guide](https://help.aliyun.com/zh/model-studio/real-time-speech-recognition-user-guide)
- [Native Flash API](https://help.aliyun.com/zh/model-studio/non-real-time-speech-recognition-for-fun-asr-flash)
- [Native recognition guide](https://help.aliyun.com/zh/model-studio/non-realtime-speech-recognition-user-guide)
- [ASR model specifications](https://help.aliyun.com/zh/model-studio/asr-model/)
- [Model Studio regional Base URLs](https://help.aliyun.com/zh/model-studio/base-url)
- [Obtain Workspace ID](https://help.aliyun.com/zh/model-studio/obtain-the-app-id-and-workspace-id)
- [API key acquisition](https://help.aliyun.com/zh/model-studio/get-api-key)
- [Workspace permission management](https://help.aliyun.com/zh/model-studio/permission-management-overview)
