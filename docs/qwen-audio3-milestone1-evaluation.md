# Qwen-Audio-3 Milestone 1 Evaluation

Initial evaluation date: 2026-08-02

Fixed-corpus vocabulary A/B date: 2026-08-05

This report contains aggregate results. It does not include audio, normal recognized or refined text, credentials, endpoints, window/application data, prompt context, or vocabulary terms. The fixed corpus, references, per-request transcripts, and provider errors remain in a private local directory excluded from Git. Diagnostics may retain a bounded upstream error identifier verbatim for operational accuracy; none of the measured sessions or A/B requests had an ASR failure.

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

## Fixed-corpus vocabulary A/B

A private, opt-in corpus replayed identical WAV files under every condition. It contains 30 single-speaker clips totaling 147.98 seconds: 10 examples for each of two technical terms and 10 negative controls, including two deliberately sound-alike controls. Language hints, models, sentence-boundary controls, audio, and all other request fields were fixed. The only changes were no vocabulary, weight 5, and weight 50. Streaming and Native each processed every clip, producing 180 successful requests with no errors, timeouts, or retries.

Recall is exact after Unicode compatibility normalization, case folding, and removal of punctuation and whitespace. Normalized CER is aggregated across all 30 references in each condition. Latency uses nearest-rank p50 and p95 and includes local command setup and completion.

| Vocabulary | Mode | Term A recall | Term B recall | Negative-control insertion | Normalized CER | Latency p50 | Latency p95 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| None | Streaming | 0/10 | 1/10 | 0/10 | 8.12% | 1,177 ms | 2,713 ms |
| None | Native | 0/10 | 2/10 | 0/10 | 7.32% | 870 ms | 1,007 ms |
| Weight 5 | Streaming | 0/10 | 1/10 | 0/10 | 7.32% | 1,163 ms | 2,034 ms |
| Weight 5 | Native | 0/10 | 2/10 | 0/10 | 7.32% | 879 ms | 1,039 ms |
| Weight 50 | Streaming | 10/10 | 10/10 | 0/10 | 0.16% | 1,291 ms | 1,908 ms |
| Weight 50 | Native | 10/10 | 10/10 | 1/10 | 0.32% | 868 ms | 1,086 ms |

Weight 5 did not improve exact target recall over the no-vocabulary condition. Weight 50 produced complete target recall in this corpus and nearly eliminated aggregate character error. Native weight 50 also converted one deliberately sound-alike negative phrase into the configured term. Streaming weight 50 did not insert either term into any negative control. Because the corpus covers one speaker, two terms, and only 10 negative controls, these measurements establish behavior for this corpus rather than a general false-positive rate. The production adaptive policy normally skips Native for healthy short streams, reducing exposure to the observed Native-only tradeoff.

## Approximate request cost

Pricing assumptions use the official Beijing rates published on 2026-08-01:

- Streaming: ¥0.00033 per input second
- Short Flash native: ¥0.00022 per input second

Based on instructed rather than instrumented recording durations, the seven sessions contained approximately 134 streaming seconds and 116 native seconds. Estimated request cost is therefore approximately:

- Streaming: ¥0.0442
- Native: ¥0.0255
- Combined: ¥0.0697

The estimate excludes free quota and is not billing evidence. This test set is dominated by deliberate silence and long recordings that correctly trigger adaptive native processing, so its 57.1% invocation rate should not be treated as a normal-use forecast.

The fixed-corpus A/B replayed 443.94 seconds through each model family. At the same published rates, its estimated request cost is ¥0.1465 for Streaming plus ¥0.0977 for Native, or ¥0.2442 combined. This estimate also excludes free quota and is not billing evidence.

## Milestone decisions

1. **Language hints — retain.** The real APIs accepted the fields, and controlled Chinese/English/mixed speech completed without failure. Keep the feature explicit and disabled by default so automatic detection remains available.
2. **Heartbeat — retain.** A silent session longer than 60 seconds completed normally without a streaming timeout. Keep it explicit and disabled by default.
3. **Dynamic vocabulary — retain with corpus-bounded claims.** Weight 5 did not improve exact recall for either tested term. Weight 50 improved both terms to 10/10 in Streaming and Native, compared with 0/10 and 1–2/10 without vocabulary. Keep weight 50 for these tested terms while disclosing the 1/10 Native sound-alike insertion and avoiding generalization to other terms or speakers. Per-application profiles remain deferred.
4. **Adaptive native final pass — retain.** Healthy short streams avoided native latency and requests; empty and long-duration cases invoked native correctly. Record the non-speech false positive as input for Milestone 2 VAD/timestamp work rather than adding transcript-dependent heuristics now.

## Closeout additions

The deterministic closeout did not rerun the original seven-session evaluation, and its timing values above remain unchanged. Local closeout added tests through the production-used Audio3 client state machine for startup and finalization deadline expiration, 65 simulated heartbeat-enabled seconds, finish completion, and cancellation. Scripted provider events validate client behavior without wall-clock sleeps or a network; the earlier real 65-second session remains the evidence for remote-provider behavior. Closeout also corrected committed-only and terminal-only streaming telemetry, advanced diagnostics to schema 3, and included validated `max_sentence_silence` and semantic-punctuation configuration foundations at user direction. The subsequent fixed-corpus vocabulary A/B is reported separately above.

Milestone 2 item 5 remains in progress: presets, multi-threshold/noise controls, and full pause/noise evaluation are not complete.

## Remaining limitations

- The fixed corpus is retained only in a private local Git-excluded directory. It covers one speaker, two technical terms, 10 examples per term, and 10 negative controls; broader speaker, pronunciation, term, and noise coverage remains unmeasured.
- The pre-milestone baseline has only one diagnostic sample.
- Per-application vocabulary profiles remain deferred.
- Noise behavior requires a larger repeatable corpus before changing defaults or policy.
