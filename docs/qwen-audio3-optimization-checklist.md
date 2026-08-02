# Qwen-Audio-3 Optimization Checklist

This document tracks the next Qwen-Audio-3 improvements for Voice Input. Update the checkboxes and work log in the same commit as each implementation change.

## Status

- [ ] Not started
- [~] In progress
- [x] Completed and validated
- [!] Blocked or deferred; record the reason in the work log

## Guardrails

- Preserve `alibaba-qwen-realtime`, `alibaba-qwen-audio3`, and `local-cli` provider behavior unless a task explicitly changes it.
- Keep experimental capabilities behind explicit configuration and retain safe defaults.
- Never include audio, transcripts, credentials, endpoints, provider response text, window content, or prompt context in diagnostics or routine logs.
- Add deterministic protocol and policy tests before live-provider validation.
- Use official Alibaba Cloud documentation as the source of truth; record any API ambiguity or provider behavior discovered during implementation.
- Do not broaden a milestone merely because a related API field exists.

## Milestone 1 — Low-risk, measurable improvements

Complete items 1–4, then stop and evaluate their combined effect before starting item 5.

### 1. `language_hints`

- [ ] Confirm the current official parameter shape, supported codes, and four-language limit.
- [ ] Define how Voice Input language settings map to ASR hints while preserving automatic detection when no hint is configured.
- [ ] Add validated configuration and Settings UI behavior.
- [ ] Send hints in streaming and native requests where officially supported.
- [ ] Add request-envelope, configuration migration, validation, and provider-compatibility tests.
- [ ] Confirm mixed Chinese/English and single-language behavior with opt-in live tests.

### 2. `heartbeat`

- [ ] Confirm heartbeat request semantics and provider timeout behavior.
- [ ] Add the streaming request parameter with a safe default.
- [ ] Verify long-silence sessions remain cancellable and do not alter normal completion semantics.
- [ ] Add deterministic request-envelope and long-idle lifecycle tests.
- [ ] Confirm no regression in shutdown latency or worker cleanup.

### 3. Dynamic hotwords (`vocabulary`)

- [ ] Confirm term limits, weight range, super-hotword behavior, precedence, and request schema.
- [ ] Design bounded global and optional per-application configuration without automatically collecting private text.
- [ ] Validate and normalize terms locally; define duplicate-term and invalid-weight behavior.
- [ ] Add Settings UI controls that make all remotely sent terms visible to the user.
- [ ] Send dynamic vocabulary only when explicitly configured.
- [ ] Add serialization, bounds, privacy, request-envelope, and legacy-provider isolation tests.
- [ ] Evaluate proper nouns and technical terms against a fixed opt-in corpus.

### 4. Adaptive native final pass

- [ ] Define explicit modes: streaming only, adaptive, and always run native final pass.
- [ ] Specify the adaptive decision table using observable states only: empty, degraded, interrupted, overloaded, missing completion, duration, and explicit accuracy request.
- [ ] Define migration from the existing boolean native-final-pass setting without surprising current users.
- [ ] Preserve deterministic result precedence and local fallback behavior.
- [ ] Expose bounded diagnostics for the decision without recording transcript content.
- [ ] Add exhaustive policy, migration, cancellation, timeout, and output-preservation tests.
- [ ] Measure native invocation rate, ASR latency, failure rate, and estimated request cost.

### Milestone 1 evaluation gate

- [ ] Run the locked local validation suite and QML validation.
- [ ] Run controlled live tests for Chinese, English, mixed language, proper nouns, silence, noise, short speech, and longer dictation.
- [ ] Compare baseline and milestone results using median and p95 streaming-ready, streaming-finalize, native, and total-ASR latency.
- [ ] Compare empty-result, degraded-streaming, native-invocation, and local-fallback rates.
- [ ] Evaluate accuracy only with an explicit local test corpus; do not store transcript/reference pairs in diagnostics or commit them to the repository.
- [ ] Record pricing assumptions, region, model IDs, test date, and sample counts.
- [ ] Decide whether to retain, revise, or revert each of items 1–4 before starting milestone 2.

## Milestone 2 — Advanced recognition controls

### 5. VAD and sentence-boundary settings

- [ ] Add validated advanced controls for `max_sentence_silence`, semantic punctuation, multi-threshold mode, and speech/noise threshold only where officially supported.
- [ ] Define presets for low-latency dictation and longer-form speech instead of exposing unexplained raw values by default.
- [ ] Test endpoint latency, long pauses, background noise, cancellation, and final transcript assembly.

### 6. Parse and use timestamps

- [ ] Parse sentence and word/character timestamps without changing transcript output.
- [ ] Validate monotonicity and bound stored in-memory metadata.
- [ ] Use timestamps for duplicate suppression, partial/final assembly, and future reconnect recovery.
- [ ] Keep timestamp diagnostics aggregate-only and transcript-free.

### 7. Workspace regional endpoints

- [ ] Add explicit Beijing/Singapore region selection and derive official workspace endpoints safely.
- [ ] Validate workspace identifiers and regional credential expectations without logging endpoint or credential values.
- [ ] Preserve custom endpoints for testing and compatibility.
- [ ] Document migration from legacy DashScope endpoints.

## Milestone 3 — Context, resilience, and separate workflows

### 8. Context enhancement and streaming reconnect

- [ ] Design an explicit-consent context model; never ingest clipboard, window, transcript history, or agent context automatically.
- [ ] Bound and visibly disclose all context sent through initial requests or `continue-task`.
- [ ] Design reconnect/replay with retained audio, timestamp-based deduplication, retry limits, and cancellation safety.
- [ ] Add deterministic disconnect, replay, duplicate, timeout, and privacy tests before live testing.

### 9. Filetrans file-transcription workflow

- [ ] Keep Filetrans separate from push-to-talk voice input.
- [ ] Define asynchronous task creation, polling/callback behavior, cancellation, file limits, retention, and cleanup.
- [ ] Add optional diarization and speaker-count controls only for supported inputs.
- [ ] Ensure local files and transcripts are never uploaded without an explicit command and confirmation.
- [ ] Add a dedicated CLI/UI workflow, documentation, and privacy-safe diagnostics.

## Work log

| Date | Item | Status | Notes |
| --- | --- | --- | --- |
| 2026-08-02 | Planning | Completed | Created the feature branch, isolated worktree, three-milestone checklist, and Milestone 1 evaluation gate from `main` at `b28af17`. No implementation started. |

## Official references

- [ASR models and specifications](https://help.aliyun.com/zh/model-studio/asr-model/)
- [Real-time speech recognition](https://help.aliyun.com/zh/model-studio/real-time-speech-recognition-user-guide)
- [Streaming client events and parameters](https://help.aliyun.com/zh/model-studio/fun-asr-client-events)
- [Streaming server events](https://help.aliyun.com/zh/model-studio/fun-asr-server-events)
- [Short-audio Flash API](https://help.aliyun.com/zh/model-studio/non-real-time-speech-recognition-for-fun-asr-flash)
- [Filetrans HTTP API](https://help.aliyun.com/zh/model-studio/fun-asr-recorded-speech-recognition-http-api)
- [Model pricing](https://help.aliyun.com/zh/model-studio/model-pricing)
