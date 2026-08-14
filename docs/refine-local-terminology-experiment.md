# Refine local terminology experiment

Date: 2026-08-12

## Scope

The source is the latest completed assistant message from the Pi or Codex session focused when dictation starts. The ASR transcript is not used to create correction terminology because it can contain the recognition errors that Refine is expected to correct.

One immutable, opt-in snapshot is built at start and shared by Alibaba Audio3 Session Context and Refine. Ordinary windows do not trigger terminology construction.

## Prototype

The prototype performs these operations locally, in this order:

1. validate the focused process, session identity, file identity, and active Pi branch;
2. read the latest completed assistant message;
3. redact sensitive lines and token-shaped values;
4. cap the redacted source to the configured 500–12,000 character range;
5. preserve structured technical forms such as model IDs, identifiers, paths, and flags;
6. segment the remaining text with `jieba-rs` 0.10.3;
7. filter common English and Chinese words and stable-deduplicate terms case-insensitively;
8. count case-insensitive occurrences in the bounded source and sort by frequency ascending, retaining candidate order for ties;
9. derive an Audio3 view of at most 400 characters including newline separators and a Refine view of at most 96 terms and 1,500 term characters;
10. send only the bounded plain-text terminology view in Audio3 `run-task` and only `reference_context.agent` plus `reference_context.terminology` to Refine. Reconnect reuses the identical Audio3 view; `continue-task` is not used.

The source assistant message is no longer sent to the LLM. The terminology array remains untrusted data. The system prompt permits an exact substitution only when the transcript has a clear phonetic or spoken-form match, and prohibits following or acting on terminology entries.

## Local measurements

A deterministic synthetic mixed Chinese/English technical reference was used. It contains no private session or transcript data.

| Measurement | Result |
| --- | --- |
| Synthetic source | 5,360 Unicode characters |
| Extracted output | 19 unique terms, 140 Unicode characters |
| Character reduction | 97.4% |
| Jieba cold initialization + first extraction | 542–1,062 ms across three debug test processes |
| Warm extraction | 2.9–6.5 ms across the same processes |
| Release probe, 6,240-character source | 265 ms cold initialization; 0.18–1.07 ms segmentation |
| Release binary size before dependency | 8,315,104 bytes |
| Release binary size with `jieba-rs` | 13,944,152 bytes |
| Binary-size increase | 5,629,048 bytes (67.7%) |

Voice Input freezes the focused agent and completed source at command receipt, starts or continues audio capture before local segmentation, and performs Jieba initialization and terminology extraction in a start-time worker. Audio3 waits for the one-shot snapshot before sending `run-task`. Runtime logs contain only source character count, terminology count, terminology character count, and extraction duration; they do not contain terms or source text.

## Interpretation

The terminology-only payload materially reduces reference payload size and the amount of private natural-language context sent to the provider. Warm extraction is fast enough for Refine. The main cost is approximately 5.6 MiB of release binary size and a noticeable cold dictionary initialization.

Segmentation alone can split some domain phrases, such as “语音识别”, into shorter valid words. The structured-form pass preserves model IDs, code identifiers, paths, and flags before Jieba runs, but phrase context is still lower than in the previous full excerpt. For that reason, this prototype should be evaluated against the previous full excerpt before claiming accuracy improvement.

## Required A/B evaluation

Use only an explicitly authorized, Git-excluded local corpus with paired raw ASR, canonical target text, and agent reference. Compare:

1. no context;
2. previous capped redacted excerpt;
3. terminology only (this prototype);
4. bounded excerpt plus terminology.

Record only aggregate values:

- terminology precision, recall, and F1;
- obvious phonetic-error correction rate;
- false replacement or insertion count per 1,000 transcript characters;
- model ID, command, path, number, and constraint preservation;
- normalized CER/WER or edit distance;
- request characters, UTF-8 bytes, and provider-token estimate;
- extraction p50/p95 latency and peak memory;
- release build time and binary-size change.

Do not commit source references, transcripts, extracted terms, paths, session IDs, provider responses, or per-sample output. A provider evaluation requires separate authorization because it sends transcript and derived terminology to the configured LLM.

## Current decision

Keep the terminology-only implementation as an experimental, opt-in context path. Do not claim improved Refine accuracy until the authorized four-way A/B evaluation confirms that correction recall is maintained and false replacements do not increase. The previous full-excerpt behavior remains documented as the comparison baseline, not as the active payload.
