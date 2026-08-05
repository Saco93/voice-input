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
- Never include audio, credentials, endpoints, window/application data, prompt context, or normal recognized/refined text in diagnostics or routine logs. Bounded upstream error fields may be recorded verbatim for operational accuracy.
- Add deterministic protocol and policy tests before live-provider validation.
- Use official Alibaba Cloud documentation as the source of truth; record any API ambiguity or provider behavior discovered during implementation.
- Do not broaden a milestone merely because a related API field exists.

## Milestone 1 — Low-risk, measurable improvements

Complete items 1–4, then stop and evaluate their combined effect before starting item 5.

### 1. `language_hints`

- [x] Confirm the current official parameter shape, supported codes, and four-language limit. Both streaming and short Flash accept `language_hints` in their `parameters` object; omission preserves automatic detection. Supported codes are `zh en ja ko vi th id ms tl hi ar fr de es pt ru it nl sv da fi no el pl cs hu ro bg hr sk`.
- [x] Define how Voice Input language settings map to ASR hints when the explicit switch is enabled. English maps to `en`, Simplified/Traditional Chinese to `zh,en`, Japanese to `ja,en`, and Korean to `ko,en`; leaving the switch disabled omits the field and preserves automatic detection.
- [x] Add validated configuration and an explicit opt-in Settings switch, disabled by default for existing and new configurations.
- [x] Send hints in streaming and native requests only when the switch is enabled.
- [x] Add request-envelope, configuration migration, validation, and provider-compatibility tests.
- [x] Confirm mixed Chinese/English and single-language behavior with opt-in live tests.

### 2. `heartbeat`

- [x] Confirm heartbeat request semantics and provider timeout behavior. Streaming accepts a boolean `heartbeat` in `payload.parameters`, defaulting to `false`. Official wording is inconsistent about zero-frame idle periods, so Voice Input must use the stricter guarantee: the flag is effective while correctly formatted silent audio continues to be sent.
- [x] Add the streaming request parameter as an explicit opt-in switch, disabled by default while continuing to send the boolean in every streaming request.
- [x] Verify long-silence sessions remain cancellable and do not alter normal completion semantics. A controlled silent session longer than 60 seconds completed normally with an empty result.
- [x] Add deterministic request-envelope tests.
- [x] Add lifecycle tests without wall-clock sleeps. Scripted in-memory provider events and a manual deadline clock now validate the production-used client state machine for startup and finalization deadline expiration with stable timeout errors and socket close, plus 65 simulated seconds including heartbeat filtering, all PCM delivery, finish completion, and cancellation observed on the next retryable read/poll. Setting the abort flag does not itself wake a blocked read; production polling remains bounded by the configured 50 ms socket read timeout. This validates client lifecycle behavior; the separate real 65-second test validates remote-provider behavior.
- [x] Confirm no observed regression in shutdown latency or worker cleanup during controlled short, long, empty, and 65-second silent sessions.

### 3. Dynamic hotwords (`vocabulary`)

- [x] Confirm term limits, weight range, super-hotword behavior, precedence, and request schema. Streaming and short Flash accept an object mapping term text to integer weight in their `parameters` object. Each request permits at most 2,000 unique terms; weights are `1–5` or exactly `50`, with at most 50 weight-50 terms. A term containing non-ASCII characters is limited to 15 total characters; a pure-ASCII term is limited to 7 space-separated segments. Dynamic `vocabulary` takes precedence over `vocabulary_id` when both are present. Singapore child-workspace support is officially ambiguous, so Milestone 1 treats hotwords as Beijing-confirmed only.
- [!] Design bounded global and optional per-application configuration without automatically collecting private text. Milestone 1 implements only bounded global entries. Any future profile must exact-match a locally captured application class, override same-named global terms locally, and never send or log the class or title. Focused-window capture is deliberately deferred.
- [x] Validate and trim terms locally; define duplicate-term and invalid-weight behavior. No normalization beyond trimming is performed.
- [x] Add Settings UI controls that make all remotely sent terms visible to the user.
- [x] Send dynamic vocabulary only when explicitly configured.
- [x] Add serialization, bounds, privacy, request-envelope, and legacy-provider isolation tests.
- [x] Evaluate proper nouns and technical terms against a fixed opt-in corpus. A private local Git-excluded corpus replayed 30 identical WAV files across no vocabulary, weight 5, and weight 50 in both Streaming and Native, for 180 successful requests. Weight 5 did not improve exact recall; weight 50 improved both tested terms to 10/10 in both modes. Streaming had 0/10 negative-control insertions, while Native converted 1/10 deliberately sound-alike controls into a configured term. The corpus covers one speaker and two terms, so claims remain corpus-bounded; details are in `docs/qwen-audio3-milestone1-evaluation.md`.

### 4. Adaptive native final pass

- [x] Define explicit modes: streaming only, adaptive, and always run native final pass.
- [x] Specify the adaptive decision table using observable states only: empty, degraded, interrupted, overloaded, missing completion, duration, and explicit accuracy request.
- [x] Define migration from the existing boolean native-final-pass setting without surprising current users.
- [x] Preserve deterministic result precedence and local fallback behavior.
- [x] Expose bounded diagnostics for the decision without recording transcript content.
- [x] Add exhaustive policy, migration, cancellation, timeout, and output-preservation tests through the production-used pass-planning and injectable invocation seams. Full live daemon orchestration remains part of the evaluation gate; deterministic invocation, suppression, and failure/result orchestration are covered locally.
- [x] Measure native invocation rate, ASR latency, failure rate, and estimated request cost. Aggregate transcript-free results are recorded in `docs/qwen-audio3-milestone1-evaluation.md`.

### Milestone 1 evaluation gate

- [x] Run the locked local validation suite and QML validation.
- [x] Run controlled live tests for Chinese, English, mixed language, configured technical terms, silence, noise, short speech, longer dictation, and silence beyond 60 seconds.
- [!] Compare baseline and milestone results using median and p95 streaming-ready, streaming-finalize, native, and total-ASR latency. Milestone medians/p95 are recorded for seven sessions, but the pre-milestone baseline has only one diagnostic sample; no statistically valid p95 baseline comparison is claimed.
- [x] Compare empty-result, degraded-streaming, native-invocation, and local-fallback rates.
- [x] Evaluate accuracy only with an explicit local test corpus; do not store transcript/reference pairs in diagnostics or commit them to the repository. The authorized corpus, references, and per-request results remain private and Git-excluded; only aggregate recall, false insertion, normalized CER, latency, and request counts are committed.
- [x] Record pricing assumptions, region, model families, test date, and sample counts.
- [x] Decide whether to retain, revise, or revert each of items 1–4 before starting milestone 2. Retain all four as explicit opt-in controls; carry the noise false-positive finding into VAD/timestamp work. Retain weight 50 for the two corpus-tested terms with the documented Native sound-alike tradeoff; do not generalize the result to other terms or speakers.

## Milestone 2 — Advanced recognition controls

Provider-facing decisions, confirmed fields, endpoint constants, ambiguities, and stop conditions are recorded in `docs/qwen-audio3-milestone2-api-decisions.md`.

### 5. VAD and sentence-boundary settings

- [~] API verification complete; runtime implementation not started. Validated `max_sentence_silence` and semantic-punctuation fields were included in Milestone 1 closeout. Milestone 2 will add the documented `multi_threshold_mode_enabled` boolean and optional `speech_noise_threshold` float; separate speech/noise threshold fields do not exist in the Audio3 documentation and are blocked. Presets and controlled evaluation remain incomplete.
- [ ] Define presets for low-latency dictation and longer-form speech instead of exposing unexplained raw values by default.
- [ ] Complete pause/noise evaluation across endpoint latency, long pauses, background noise, cancellation, and final transcript assembly.

### 6. Parse and use timestamps

- [ ] Parse sentence and timed word/segment ranges without changing transcript output; the provider does not guarantee one timestamp per Unicode character.
- [ ] Validate event-local range monotonicity and bound stored in-memory metadata without assuming monotonic revisions.
- [!] Keep transcript assembly unchanged initially. Identity-dependent duplicate suppression and correction replacement require an official stable revision contract or an algorithm proven correct when IDs/ranges are revised; finite live captures cannot establish that guarantee. Reconnect/replay remains Milestone 3.
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
| 2026-08-02 | Milestone 1 API verification | Completed | Confirmed the official request placement, language-code allowlist, heartbeat semantics, and dynamic-vocabulary limits for the streaming and short Flash models. Recorded the Singapore hotword-support ambiguity; no inference request was made. |
| 2026-08-02 | Milestone 1 items 1–3 implementation | Completed with documented follow-ups | Implemented opt-in language hints, opt-in explicit streaming heartbeat, and bounded global dynamic vocabulary for streaming and native requests. Added Settings controls, migration/default/validation/request/privacy/isolation tests, sample configuration, and documentation. Per-application profiles remain deferred, and fixed-corpus vocabulary A/B measurement remains under discussion. |
| 2026-08-02 | Milestone 1 item 4 implementation | Completed locally | Replaced the Audio3 native-pass boolean with streaming-only, adaptive, and always modes; migrated legacy `true` to always and `false` to streaming-only. Added a text-free 30-second adaptive policy, explicit completion tracking, privacy-safe diagnostics schema v2, Settings/QML controls, bilingual documentation, and exhaustive migration/policy/result tests through production-used planning and injectable invocation seams. |
| 2026-08-02 | Milestone 1 live evaluation | Completed with documented limits | Real streaming/native APIs accepted the new controls. Seven controlled, transcript-free diagnostic samples covered short Chinese/English/mixed speech, configured technical terms, silence, repeated noise, long speech, and silence beyond 60 seconds. Healthy short streams skipped native; empty and long cases invoked it; no fallback or ASR failure occurred. One of two noise attempts produced a false positive. Aggregate timings, invocation rate, approximate Beijing pricing, decisions, and limitations are in `docs/qwen-audio3-milestone1-evaluation.md`. |
| 2026-08-05 | Milestone 1 closeout | Completed locally; no new live evaluation | Added deterministic production-used Audio3 lifecycle coverage for 65 simulated heartbeat-enabled seconds and cancellation, corrected terminal/committed-only streaming telemetry, made malformed persisted provider error codes degrade to unavailable, and advanced diagnostics to schema v3. Included validated `max_sentence_silence` and semantic-punctuation configuration foundations at user direction; Milestone 2 item 5 remains in progress because presets, multi-threshold/noise controls, and full pause/noise evaluation are still outstanding. |
| 2026-08-05 | Fixed-corpus vocabulary A/B | Completed | Retained a private local Git-excluded corpus with 30 clips and replayed identical audio under no vocabulary, weight 5, and weight 50 in Streaming and Native. All 180 requests succeeded. Weight 50 produced 10/10 recall for each tested term in both modes; weight 5 showed no recall uplift. Streaming had no negative-control insertion, while Native weight 50 inserted a configured term for one deliberately sound-alike control. Only aggregate results are committed. |
| 2026-08-05 | Milestone 2 Phase 0 API verification | Completed; implementation not started | Confirmed the Audio3 VAD field schema, timestamp paths and units, Beijing/Singapore endpoint matrix, hostname-based workspace routing, and region/workspace credential scope from official documentation. Separate speech/noise thresholds and guaranteed character timestamps are unsupported. Identity-dependent timestamp deduplication remains blocked without an official revision contract or a revision-safe algorithm; finite captures are observational only. Workspace ID has no published business grammar, so only hostname transport safety can be validated. Decisions and live-test gates are in `docs/qwen-audio3-milestone2-api-decisions.md`. |

## Official references

- [ASR models and specifications](https://help.aliyun.com/zh/model-studio/asr-model/)
- [Real-time speech recognition](https://help.aliyun.com/zh/model-studio/real-time-speech-recognition-user-guide)
- [Streaming client events and parameters](https://help.aliyun.com/zh/model-studio/fun-asr-client-events)
- [Streaming server events](https://help.aliyun.com/zh/model-studio/fun-asr-server-events)
- [Short-audio Flash API](https://help.aliyun.com/zh/model-studio/non-real-time-speech-recognition-for-fun-asr-flash)
- [Filetrans HTTP API](https://help.aliyun.com/zh/model-studio/fun-asr-recorded-speech-recognition-http-api)
- [Model pricing](https://help.aliyun.com/zh/model-studio/model-pricing)
