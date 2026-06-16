# TTS English-Only Fallback (lean v2)

## Objective

When an English-only cloud TTS voice receives non-English text, stop emitting
gibberish. Do it with **minimal metadata, no new config knobs, and negligible
latency** — automatically route that one utterance to the local multilingual
engine, and degrade gracefully when local TTS isn't available.

## Design answers to the three review points

1. **Metadata maintenance — keep it to one boolean.** Instead of a rich
   `Multilingual / English / Set(list)` descriptor, add a single
   `english_only: bool` to the TTS catalogue entry
   (`TtsDefaults`, `crates/fono-core/src/provider_catalog.rs:94-108`).
   - It defaults to `false` (multilingual), so only the ~4 English-only entries
     must be flagged: Groq Orpheus
     (`crates/fono-core/src/provider_catalog.rs:286-314`), Deepgram aura
     (`:460-468`), Speechmatics preview (`:553-565`), and OpenRouter Grok-voice
     after verification (`:423-443`). OpenAI and Cartesia stay `false`.
   - This mirrors the STT side's existing `multilingual: bool`
     (`crates/fono-stt/src/registry.rs:167`) — a shape the codebase already
     maintains comfortably. A new provider only ever sets one bool; forgetting
     it fails safe (assumed multilingual, i.e. current behaviour).

2. **No new knobs; cheap language resolution.** Drop the
   `[tts].on_language_mismatch` config entirely — the behaviour is hardcoded
   (fallback-local, else skip-with-warning).
   - **Cost of resolving the target language:** for the assistant path it is
     **zero** — the language is already known as `tts_lang`
     (`crates/fono/src/assistant.rs:472-476`), derived from STT detection.
   - For the `fono speak` / MCP paths (which today pass `lang = None`:
     `crates/fono/src/speak_stream.rs:109`,
     `crates/fono-mcp-server/src/voice_io.rs:479`), `whatlang` is the only place
     detection runs. `whatlang` is a pure-Rust trigram classifier with **no
     model files**, already a dependency of `fono-tts`
     (`crates/fono-tts/Cargo.toml:67`); a sentence classifies in **well under
     1 ms** — negligible next to a multi-hundred-ms TTS synthesis + network
     round-trip. It is the same detector already used in the local router
     (`crates/fono-tts/src/local_router.rs`) and polish
     (`crates/fono-polish/src/traits.rs`).
   - **Other options considered (and why whatlang wins):**
     - *Use `general.languages` only* — free, but static: can't tell which
       language a given utterance is, so a bilingual EN+RO user breaks.
     - *Ask the STT/LLM for the language* — already covers the assistant path
       (no detection needed); offers nothing for the generic speak path.
     - *No detection, route by config* — only safe for the single-language
       user; fails the mixed-language case the bug is about.
     - **Decision:** reuse the known signal where it exists (assistant), and
       fall back to the already-present, sub-millisecond `whatlang` only on the
       generic speak/MCP paths.

3. **Local fallback is the agreed behaviour.** When the configured cloud TTS is
   `english_only` and the resolved target language is non-English, synthesize
   that utterance with the local Piper multilingual voice instead of the cloud
   backend. This reuses the routing already living in
   `crates/fono-tts/src/local_router.rs`.

## Assumptions

- "English" detection only needs a yes/no answer ("is this English?"), so a
  low-confidence `whatlang` result is treated as "assume English / speak
  anyway" — never forces a fallback on a one-word utterance.
- The local Piper engine may be unavailable (`tts-local` feature off, or the
  voice not yet downloaded); the design must degrade without crashing.
- No user-facing configuration is added; the behaviour is automatic and
  self-explanatory.

## Implementation Plan

- [ ] Task 1. Add `english_only: bool` (default `false`) to `TtsDefaults`
  (`crates/fono-core/src/provider_catalog.rs:94-108`) and set it `true` on the
  Groq, Deepgram, Speechmatics, and (after doc verification) OpenRouter TTS
  entries. Pin the values with a small unit test, matching the style of
  `assistant_multimodal_and_web_search_pinned`
  (`crates/fono-core/src/provider_catalog.rs:848`).

- [ ] Task 2. Add a tiny helper, e.g.
  `fn tts_backend_english_only(backend: &TtsBackend) -> bool`, reading the
  catalogue, so consumers don't duplicate the lookup.

- [ ] Task 3. Resolve the target language per utterance only where it isn't
  already known:
  - Assistant: reuse the existing `tts_lang`
    (`crates/fono/src/assistant.rs:472-476`) — no new work.
  - `fono speak --stream` and MCP `speak_text`: run `whatlang` on the
    already-sanitised text, but only when the active backend is `english_only`
    (skip detection entirely for multilingual backends, so the common path pays
    nothing).

- [ ] Task 4. Add the fallback at one chokepoint. When the active backend is
  `english_only` **and** the resolved language is reliably non-English, route
  that utterance through the local Piper router
  (`crates/fono-tts/src/local_router.rs`) instead of the cloud backend.
  - Build/keep the local engine handle lazily and cache it so repeated
    non-English utterances don't reload the model on the hot path.

- [ ] Task 5. Graceful degrade: if the local engine isn't available
  (feature off / voice missing), log + surface **one** warning (reuse the
  once-per-session warning pattern at `crates/fono/src/assistant.rs:894-905`)
  and skip that utterance rather than play gibberish.

- [ ] Task 6. Tests:
  - Catalogue `english_only` values pinned per provider.
  - `whatlang`-driven decision: English text → cloud backend used; reliable
    non-English → local route chosen; low-confidence short text → cloud
    (speak-anyway).
  - Degrade path: english_only + non-English + no local engine → warn + skip,
    no cloud call with foreign text.

- [ ] Task 7. Docs: note the automatic behaviour in `docs/providers.md` (the
  TTS section already flags Speechmatics English-only near
  `docs/providers.md:529`); add a CHANGELOG entry; update `docs/status.md`.

## Verification Criteria

- Non-English text sent to an English-only cloud voice is spoken by the local
  multilingual engine, or cleanly skipped with one warning when local TTS is
  unavailable — never English-phonemized gibberish.
- Multilingual backends (OpenAI, Cartesia) and the local router are unchanged
  and pay **no** detection cost (detection is gated behind `english_only`).
- No new config keys; the only schema change is one catalogue boolean.
- Added latency on the affected path is dominated by `whatlang` (< 1 ms);
  the common multilingual path is untouched.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D
  warnings`, and `cargo test --workspace` pass.

## Potential Risks and Mitigations

1. **Wrong `english_only` flag for a provider.** Mitigation: verify against
   provider docs, pin with a test, default to `false` (fail safe = current
   behaviour).
2. **`whatlang` mis-detects a short utterance.** Mitigation: require a reliable
   detection (reuse the gate at `crates/fono-polish/src/traits.rs:274`); on low
   confidence, speak anyway via the cloud backend.
3. **Local engine unavailable.** Mitigation: Task 5 warn-and-skip degrade; no
   crash, no gibberish.
4. **Latency creep.** Mitigation: detection runs only when the backend is
   `english_only`; the local handle is cached after first use.

## Alternative Approaches

1. **One boolean + automatic local fallback (recommended, this plan).** Lowest
   complexity and maintenance; no knobs; intelligible output automatically.
2. **Warn-only, no fallback.** Even smaller, but degrades a working feature to
   silence rather than fixing it.
3. **Assistant `force-english` reply instead of local fallback.** Avoids
   needing local TTS, but only helps the assistant path and changes what the
   user hears vs. reads; kept as the implicit fallback rationale, not the
   default.
