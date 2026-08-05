# Qwen-Audio-3 Milestone 1 Evaluation

Date: 2026-08-02

This report contains aggregate results. It does not include audio, normal recognized or refined text, credentials, endpoints, window/application data, prompt context, or vocabulary terms. Diagnostics may retain a bounded upstream error identifier verbatim for operational accuracy; none of the measured sessions had an ASR failure.

## Candidate

- Provider: `alibaba-qwen-audio3`
- Streaming model family: Qwen-Audio-3.0 ASR Flash Streaming
- Native model family: Qwen-Audio-3.0 ASR Flash
- Native final-pass mode: `adaptive`
- Language hints: enabled
- Streaming heartbeat: enabled
- Dynamic vocabulary: 2 explicitly configured global entries
- Local fallback: enabled
- Diagnostics schema: 3
- Local validation: 199 Rust tests, Clippy with warnings denied, and QML validation

## Live test coverage

Seven controlled microphone sessions were included in the aggregate:

- short mixed-language speech with configured technical terms;
- short English speech with configured technical terms;
- short silence;
- two short non-speech noise attempts;
- speech longer than the 30-second adaptive threshold;
- silence longer than 60 seconds.

Separate public-audio tests confirmed that the real streaming API accepts language hints, heartbeat, and dynamic vocabulary together, and that the real native API accepts language hints and dynamic vocabulary. No private microphone audio was used for those protocol tests.

## Aggregate ASR timings

Nearest-rank p95 is used. Values are milliseconds.

| Stage | Samples | Median | p95 |
| --- | ---: | ---: | ---: |
| Streaming ready | 7 | 159 | 163 |
| Streaming finalize | 7 | 552 | 823 |
| Native final pass, when invoked | 4 | 2,029 | 2,791 |
| Total ASR | 7 | 1,362 | 3,199 |

A pre-milestone normal-microphone sample using always-on native final pass recorded 145 ms streaming-ready, 769 ms streaming-finalize, 807 ms native, and 1,577 ms total ASR. Its sample count is one, so it is not a statistical baseline and no p95 comparison is claimed. The two healthy short-speech milestone samples completed ASR in 823 ms and 654 ms while correctly skipping native processing.

## Decisions and outcomes

| Metric | Result |
| --- | ---: |
| Native invoked | 4/7 (57.1%) |
| Native skipped as healthy short stream | 3/7 (42.9%) |
| Empty final result | 3/7 (42.9%) |
| Degraded or failed ASR | 0/7 |
| Local fallback invoked | 0/7 |
| Long-silence connection survived beyond 60 seconds | Yes |
| Long speech invoked native because of duration | Yes |

Both short speech sessions were classified as healthy streams and skipped native processing. Silence and one noise attempt were empty and invoked native processing. The second noise attempt was incorrectly accepted as a healthy nonempty stream and produced output; the repeated noise attempt was correctly empty. This 1-of-2 false-positive observation is too small for a rate claim, but it demonstrates that adaptive policy cannot identify a semantically wrong yet protocol-healthy transcript without stronger VAD/timestamp evidence.

## Approximate request cost

Pricing assumptions use the official Beijing rates published on 2026-08-01:

- Streaming: ¥0.00033 per input second
- Short Flash native: ¥0.00022 per input second

Based on instructed rather than instrumented recording durations, the seven sessions contained approximately 134 streaming seconds and 116 native seconds. Estimated request cost is therefore approximately:

- Streaming: ¥0.0442
- Native: ¥0.0255
- Combined: ¥0.0697

The estimate excludes free quota and is not billing evidence. This test set is dominated by deliberate silence and long recordings that correctly trigger adaptive native processing, so its 57.1% invocation rate should not be treated as a normal-use forecast.

## Milestone decisions

1. **Language hints — retain.** The real APIs accepted the fields, and controlled Chinese/English/mixed speech completed without failure. Keep the feature explicit and disabled by default so automatic detection remains available.
2. **Heartbeat — retain.** A silent session longer than 60 seconds completed normally without a streaming timeout. Keep it explicit and disabled by default.
3. **Dynamic vocabulary — retain with limited claims.** The real APIs accepted the configured entries and the UI exposes every transmitted term. Accuracy benefit was not isolated with an A/B corpus; per-application profiles remain deferred.
4. **Adaptive native final pass — retain.** Healthy short streams avoided native latency and requests; empty and long-duration cases invoked native correctly. Record the non-speech false positive as input for Milestone 2 VAD/timestamp work rather than adding transcript-dependent heuristics now.

## Closeout additions

No new live evaluation was run, and the previously measured timing values above are unchanged. Local closeout added deterministic tests through the production-used Audio3 client state machine for startup and finalization deadline expiration, 65 simulated heartbeat-enabled seconds, finish completion, and cancellation. Scripted provider events validate client behavior without wall-clock sleeps or a network; the earlier real 65-second session remains the evidence for remote-provider behavior. Closeout also corrected committed-only and terminal-only streaming telemetry, advanced diagnostics to schema 3, and included validated `max_sentence_silence` and semantic-punctuation configuration foundations at user direction.

Milestone 2 item 5 remains in progress: presets, multi-threshold/noise controls, and full pause/noise evaluation are not complete.

## Remaining limitations

- No fixed opt-in transcript/reference corpus was committed or retained, so CER/WER and vocabulary uplift are not measured; a fixed-corpus A/B remains under discussion.
- The pre-milestone baseline has only one diagnostic sample.
- Per-application vocabulary profiles remain deferred.
- Noise behavior requires a larger repeatable corpus before changing defaults or policy.
