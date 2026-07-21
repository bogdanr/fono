# fono-bench

Latency + accuracy benchmark for Fono's STT/LLM pipeline.

The benchmark feeds **pre-recorded public-domain dictation clips** in
multiple languages through any `SpeechToText` (and optionally
`TextFormatter`) implementation, and emits per-clip + aggregate metrics:

* total wall-clock latency (capture-equivalent → final injectable text)
* STT-stage latency
* LLM-stage latency
* **Word Error Rate** (WER) vs the canonical transcript
* aggregate p50 / p95 per language and per provider

This crate is the home of every latency-related verification claim in
`docs/plans/2026-04-25-fono-latency-v1.md` (tasks L27–L30).

## Three layers, three audiences

| Layer | Audience | Command | What it measures |
|---|---|---|---|
| Criterion bench (`benches/orchestrator.rs`) | CI & developers | `cargo bench -p fono-bench` | Orchestrator overhead with fake STT/LLM (network-free, deterministic). p95 budget = **50 ms**. |
| Smoke integration test (`tests/latency_smoke.rs`) | CI | `cargo test -p fono-bench --release -- --ignored latency` | End-to-end correctness on synthetic PCM with a fake transcribing STT. Asserts WER == 0 and p95 < 50 ms. |
| Real-audio CLI (`fono-bench` binary) | Maintainers running release-validation | `cargo run -p fono-bench --release -- --provider groq --languages en,es,fr,de` | Real public-domain clips through real providers. WER + latency p50/p95 per language. JSON output for plotting. |

## Fixture registry (public domain)

The fixtures are sourced from the **LibriVox public-domain audiobook**
corpus and the **CC0** Wikimedia Commons voice samples. Each fixture
declares:

* a stable HTTPS URL,
* a SHA-256 pin (verified on download),
* the canonical reference transcript,
* the spoken language (BCP-47 tag),
* an approximate duration in seconds.

The registry lives in `src/fixtures.rs`. Before you run real-provider
benchmarks, populate the cache with:

```bash
crates/fono-bench/scripts/fetch-fixtures.sh
```

That script is the source of truth for which clips are pinned. The
runner refuses to execute against a fixture whose SHA-256 doesn't match,
so swapping in different recordings is a deliberate act (commit a new
manifest; CI will see the diff).

Fixtures are cached in `${XDG_CACHE_HOME:-$HOME/.cache}/fono/bench/`
with the same on-disk layout the production model downloader uses.

## How the runner works

1. Load the fixture manifest filtered by `--languages`.
2. For each fixture: ensure the WAV is on disk (download if missing,
   verify SHA-256), decode 16-bit PCM mono into `Vec<f32>`.
3. Pass the buffer through the configured `SpeechToText` and optional
   `TextFormatter`.
4. Compute WER vs the reference transcript.
5. Record per-stage latencies via `Instant::now()` deltas.
6. Aggregate to p50 / p95 per language and per provider.
7. Emit a JSON report.

The runner is intentionally I/O free for the criterion bench — fakes
implement the same traits and skip both network and disk.

## Output format (`--json`)

```json
{
  "provider_stt": "groq",
  "provider_llm": "cerebras",
  "ran_at": "2026-04-25T12:34:56Z",
  "by_language": {
    "en": { "n": 4, "wer": 0.034, "p50_total_ms": 612, "p95_total_ms": 1340, ... },
    "es": { "n": 3, "wer": 0.061, ... }
  },
  "by_clip": [ ... ]
}
```

## Regression gating

`cargo run -p fono-bench --release -- --provider groq --baseline
docs/bench/baseline-groq.json` exits non-zero if:

* any language's WER regresses > 5 percentage points, OR
* any language's p95 total latency regresses > 15 %.

The baseline files are checked in under `docs/bench/`. CI updates them
manually after a deliberate accuracy/latency change with a corresponding
PR comment.

## TTS backend benchmark (`fono-bench tts`)

Compares the three local ONNX TTS backends — **Piper**, **Kokoro**, and
**Supertonic** — on English and Romanian, to inform default-voice
decisions. Each engine is constructed **directly** (bypassing the
daemon's `LocalRouter`, which would otherwise auto-route English to
Kokoro), so you measure the backend you actually asked for.

Feature-gated on `tts-local` (off by default; pulls in `fono-tts` and a
statically linked `libonnxruntime.a`). It never touches the shipped
`fono` binary graph — `fono-bench` is `publish = false`.

```sh
# ORT_LIB_LOCATION is resolved the same way CI does.
export ORT_LIB_LOCATION="$(bash scripts/fetch-onnxruntime.sh | tail -1)"

# Full matrix, download any missing voice assets first, pretty JSON to file.
cargo run -p fono-bench --release --features tts-local -- tts \
  --languages en,ro \
  --backends piper,kokoro,supertonic \
  --download \
  --iterations 5 --warmup 1 \
  --wav-dir ./tts-bench-out \
  --machine-label "thinkpad-t14" \
  --out ./tts-bench-out/report.json --pretty
```

What it measures per (language × backend × voice):

* **cold-start** model-load latency (first-F8-press proxy) vs **warm**
  steady-state synthesis time
* **real-time factor** (synth time ÷ audio duration) — the number that
  says "does it keep up with speech", with p50 / p95 over `--iterations`
* **peak RSS** per backend, isolated one-engine-per-process and read
  from `/proc/self/status` (`VmHWM`, Linux only)
* **disk / download footprint** per backend (Piper per-voice vs Kokoro
  shared model vs Supertonic's ~140 MB pack)
* **determinism** check (Supertonic re-seeds per call → identical PCM)
  and **robustness** logging on the hard sentences
* optional **STT round-trip WER/CER** quality anchor (`--stt groq|openai|
  local|fake`) — transcribe each output back and score it against the
  input text, an objective backstop for your subjective listening

Output: one **raw, un-normalised** WAV per synth (native volume
preserved on purpose) with descriptive names
(`<lang>__<backend>__<voice>__<sentence-id>.wav`), a grouped
**listening index** for easy A/B, and a schema-versioned JSON report.

The difficult-sentence fixtures live in `tests/fixtures/tts/sentences.toml`
— tongue-twisters, numbers/dates/units, acronyms, Romanian diacritics,
and mixed-language sentences (English tech terms inside Romanian, the
coding-agent case). Kokoro is English-only, so the Romanian matrix is
Piper + Supertonic automatically.

## Adding a new fixture

1. Pick a public-domain or CC0 audio source (LibriVox, Wikimedia
   Commons, Common Voice's CC0 portion).
2. Trim to ~3–10 seconds of clear speech — `ffmpeg -ss 12.5 -t 5
   -ac 1 -ar 16000 -c:a pcm_s16le in.ogg out.wav`.
3. Type out the canonical transcript verbatim.
4. Compute SHA-256 of the WAV: `sha256sum out.wav`.
5. Add an entry to `FIXTURES` in `src/fixtures.rs`.
6. Run the smoke test and the binary against it once to confirm.

Reference texts must be the **exact** spoken words, lowercased; the WER
implementation does its own normalisation but treats letter-perfect
mismatches as edits.
