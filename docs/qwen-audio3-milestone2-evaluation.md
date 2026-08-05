# Qwen-Audio-3 Milestone 2 Evaluation

Evaluation date: 2026-08-05

Candidate commit: `8c11b9e`

This report contains aggregate results only. It does not include audio, reference text, normal or refined transcripts, provider messages, credentials, endpoints, Workspace IDs, model names, window/application data, or prompt context. Source recordings, generated WAV files, per-request transcripts, errors, detailed per-file construction metadata, and raw per-session diagnostics remain in a private local directory excluded from Git.

## Scope

The authorized evaluation covered:

- Standard, Low-latency dictation, and Long-form Streaming presets;
- exact inserted digital pauses, natural multi-sentence speech, stable background noise, one impulse-noise sample, silence, and noise-only input;
- one real microphone session per preset;
- one live cancellation canary;
- bounded timestamp parser compatibility;
- Beijing Regional routing with an empty Workspace ID for Streaming and Native.

Singapore and workspace-specific routing were not attempted because no matching region/workspace credential was configured. Their live status remains unknown.

## Fixed private corpus

Twelve private source recordings produced 17 fixed WAV files. Four independently recorded clause fragments were trimmed with a local RMS threshold and combined into nine pause samples. The inserted digital silence matrix was:

- Chinese: 250, 450, 800, 1,100, 1,600, and 2,200 ms;
- English: 450, 800, and 1,600 ms.

Inserted digital silence duration is exact and reproducible. The acoustic boundary between the last speech sound and first subsequent speech sound still depends on threshold-based trimming, so these files support controlled relative comparison rather than instrument-grade VAD threshold calibration. The corpus also included two short utterances, two natural multi-sentence utterances, speech with stable background noise, speech with one impulse noise, silence, and noise-only input.

The 17 WAV files totaled 96.97 seconds. Identical files were replayed once under each preset, totaling 290.90 input seconds and 51 requests. All 51 requests succeeded without timeout or retry.

### Aggregate results

Latency includes local command startup and rapid prerecorded-audio submission. It is not realtime endpoint latency.

| Preset | Successful | Speech nonempty | Silence/noise false positive | Normalized CER | Latency p50 | Latency p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Standard | 17/17 | 15/15 | 0/2 | 2.31% | 1,141 ms | 1,911 ms |
| Low-latency dictation | 17/17 | 15/15 | 0/2 | 1.28% | 1,189 ms | 2,411 ms |
| Long-form | 17/17 | 15/15 | 0/2 | 3.08% | 1,104 ms | 1,933 ms |

All nine pause samples retained both clause portions under all three presets. Each preset produced an exact normalized reference match on 5/9 pause files. The other results contained ordinary recognition differences of 1–5 normalized character edits; no complete clause was dropped or duplicated. Both natural multi-sentence samples and the impulse-noise speech sample matched exactly under all presets. The stable-background-noise speech sample had the same small recognition difference under all presets. Pure silence and noise-only input remained empty under all presets.

One replay per condition cannot separate preset effects from provider variability. The lower Low-latency CER in this corpus is therefore an observation, not a general accuracy claim. Rapid replay also cannot establish that one preset has lower live endpoint latency.

## Real microphone sessions

One controlled toggle-mode microphone session was completed under each preset. Each session contained two spoken clauses separated by a natural approximately 1–2 second pause. Diagnostics are measured from recording start; first-nonempty latency therefore includes user timing before and during the first clause.

| Preset | Audio sent | Ready | First nonempty partial | Segment finals | Finalize after stop | Timestamp-bearing results | Accepted timed units |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Standard | 8,948 ms | 136 ms | 1,847 ms | 2 | 652 ms | 27 | 38 |
| Low-latency dictation | 8,564 ms | 130 ms | 1,665 ms | 3 | 328 ms | 27 | 37 |
| Long-form | 8,564 ms | 127 ms | 2,188 ms | 2 | 349 ms | 19 | 31 |

All three sessions completed through Streaming and skipped the adaptive Native final pass as healthy short streams. Across the three sessions, the bounded parser accepted 73 timestamp-bearing results and 106 timed units. It rejected zero timestamp-bearing results and truncated zero units. These observations establish compatibility with the event shapes received during this evaluation. They do not establish a stable revision identity contract, monotonic sentence IDs, or character-level alignment.

Each preset has only one realtime sample, and the spoken timing was human-controlled. The apparent finalize difference is not sufficient for a stable median, p95, or recommendation.

## Cancellation

A live Long-form session was cancelled with F8 while toggle-mode recording was active. No final text was delivered. The daemon journal recorded the `cancel` control and an immediate transition to idle, with no output operation before the next session. The single-session diagnostics slot was subsequently replaced by the user's next normal recording, so no cancelled-session diagnostic snapshot is claimed. Deterministic cancellation tests continue to provide exhaustive state-machine coverage, including suppression of Native final pass and output.

## Regional route canaries

With Regional Beijing selected and Workspace ID empty, one private WAV completed successfully through each API:

| Route | Result | Command elapsed |
| --- | --- | ---: |
| Beijing Streaming legacy host | Success | 1,364 ms |
| Beijing Native legacy host | Success | 984 ms |

The existing encrypted credential was not probed against Singapore or a workspace host. No automatic fallback or cross-region attempt was made.

## Approximate request cost

Using the Beijing rates already recorded for Milestone 1—¥0.00033 per Streaming input second and ¥0.00022 per Native input second—the instrumented corpus, protocol/route canaries, and three completed realtime sessions account for approximately ¥0.113. The short cancellation input adds only a small uninstrumented amount. This estimate excludes free quota and is not billing evidence.

## Decisions

1. **Keep Standard as the default.** It preserves the pre-milestone request and completed every evaluated scenario without a silence/noise false positive.
2. **Keep Low-latency dictation as an explicit option.** Its field combination was accepted and it preserved all pause clauses. Its lower corpus CER and shorter one-sample finalize time are bounded observations, not general recommendations.
3. **Keep Long-form as an explicit option.** Its field combination was accepted, semantic punctuation produced valid results, and it preserved all pause clauses. This corpus does not show a general accuracy advantage.
4. **Keep Custom controls and validation unchanged.** The provider documentation still exposes one optional combined speech/noise threshold; separate threshold fields remain unsupported.
5. **Retain bounded timestamp parsing without identity-based transcript changes.** Live events were parser-compatible, but finite captures cannot prove stable revision identity semantics.
6. **Mark Beijing empty-workspace routing live-validated for the exercised Streaming and Native calls.** Singapore and workspace-specific routes remain pending a credential scoped to those routes; no feature-parity claim is made.
