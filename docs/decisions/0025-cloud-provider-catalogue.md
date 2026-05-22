# ADR 0025 — Cloud provider capability catalogue

- **Status:** Accepted
- **Date:** 2026-05-13
- **Supersedes:** none
- **Related:** [ADR 0024 — Assistant multimodal & web search](0024-assistant-multimodal-and-search.md)
- **Issues:** [#9](https://github.com/bogdanr/fono/issues/9) (wizard
  collapse), [#11](https://github.com/bogdanr/fono/issues/11) (multi-
  provider TTS)
- **Plan:** [`plans/2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md`](../../plans/2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md)

## Context

Before this work, capability information for every cloud provider Fono
supports was duplicated across at least seven sites:

- Five `match` blocks in `crates/fono/src/wizard.rs` (STT picker, LLM
  picker, assistant picker, the assistant TTS reuse path, and the
  Mixed-mode key-prompt fork);
- `crates/fono-core/src/providers.rs::cloud_pair`, which carried its
  own hard-coded `(SttBackend, LlmBackend)` table for `fono use cloud
  <name>`;
- `crates/fono/src/cli.rs` for the `fono doctor` tabular output.

Adding multi-provider TTS (issue #11) and multimodal/web-search
assistant extras (issue #10 / ADR 0024) would have multiplied the
duplication — each new capability dimension would have grown another
column in every `match` arm. The pain was already visible in v0.7.1's
wizard, which prompted "keep existing `GROQ_API_KEY`?" three times in
a row because the dedup logic lived inside each provider arm rather
than at the top of the flow.

## Decision

A single immutable catalogue lives at
`crates/fono-core/src/provider_catalog.rs`:

```rust
pub const CLOUD_PROVIDERS: &[CloudProvider] = &[
    CloudProvider {
        id: "openai",
        display_name: "OpenAI",
        tagline: "Flagship multimodal models with native web search and TTS.",
        console_url: "https://platform.openai.com/api-keys",
        key_env: "OPENAI_API_KEY",
        stt: Some(SttDefaults { model: "whisper-1" }),
        llm: Some(LlmDefaults { model: "gpt-5.4-nano" }),
        assistant: Some(AssistantDefaults {
            text_model: "gpt-5.4-mini",
            multimodal_model: Some("gpt-5.4-mini"),
            web_search: WebSearchSupport::NativeTool("web_search_preview"),
            badges: &[Badge::Stt, Badge::Llm, Badge::Assistant,
                      Badge::Tts, Badge::Vision, Badge::Search],
        }),
        tts: Some(TtsDefaults {
            model: "tts-1",
            default_voice: "alloy",
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://api.openai.com/v1",
            },
            runtime_probe: false,
        }),
    },
    // … one entry per cloud provider Fono talks to …
];
```

Every consumer reads from this slice:

- The wizard's primary-provider picker, secondary-STT picker, assistant
  chat picker, assistant TTS picker, and the `prefer_vision /
  prefer_web_search` extras row;
- `fono use cloud <name>` (via `cloud_pair_from_catalog`);
- `fono doctor`'s capability table;
- The assistant builder, which consults
  `assistant.multimodal_model` and `assistant.web_search` at startup.

Compile-time unit tests in `provider_catalog::tests` lock the
catalogue against drift: every entry's `key_env` must match the
canonical `*_key_env(&Backend)` from `providers.rs`, every declared
capability's id must round-trip through `parse_*_backend`, and no
cloud `*Backend` variant may exist without a matching catalogue entry.

## Alternatives considered

1. **Per-issue point fixes.** Land the wizard collapse (#9) as a
   refactor inside `wizard.rs`; land multi-provider TTS (#11) by
   threading a TTS-specific table through `fono-tts`; land
   multimodal/search by extending the assistant-builder switch. Each
   fix would have been smaller in isolation, but the three projects
   would have ended up reading three different copies of "what does
   Anthropic offer" — exactly the pre-existing drift problem at a
   larger scale. Rejected.

2. **Runtime registry built from trait objects.** A
   `dyn CloudProvider` trait with `fn capabilities(&self) ->
   Capabilities` registered into a `OnceCell<HashMap<&str, Arc<dyn
   CloudProvider>>>` at startup. Idiomatic in larger plug-in systems
   but overkill here: Fono ships every provider it knows about in the
   same binary, there is no out-of-tree plug-in surface, and the
   trait-object indirection would have made the tests above hard to
   express. Rejected.

3. **Macro-driven enum-to-table generation.** A `define_providers!
   { OpenAI { stt: "whisper-1", … }, Groq { … } }` macro emits both
   the enum variants and the catalogue. Avoids hand-keeping the two
   in sync but adds an opaque macro that contributors have to read
   to extend the catalogue, with effectively the same drift-prevention
   power as the compile-time unit tests we already have. Rejected on
   complexity grounds; reconsider if the catalogue grows past ~25
   providers.

The const slice is the smallest correct shape: contributors add one
struct literal, the compiler checks every cross-reference, and the
in-module unit tests catch the rest.

## Deferred work

Items the catalogue *can* describe today but Fono doesn't yet
runtime-wire:

- **Cartesia STT.** Wired in `crates/fono-stt/src/cartesia.rs`
  (batch `POST /stt`) per
  `plans/2026-05-23-cartesia-stt-support-v2.md`; catalogued under
  `stt: Some(SttDefaults { model: "ink-whisper" })`. `ink-2` is
  realtime-only and arrives in a Phase 2 streaming slice.
- **Cartesia TTS.** Wired in Phase F (`fono-tts::cartesia`); the
  catalogue entry is the source of truth for the voice id and the
  Sonic-2 model.
- **Azure TTS.** Catalogue stub-only today (STT-only entry). Wire
  when a contributor requests it; the Cognitive Services Speech
  endpoint shape is well-known.
- **Perplexity Sonar.** Always-search LLM that maps to
  `WebSearchSupport::Always`. Not yet catalogued; the enum variant
  exists for the day it is.
- **Gemini chat client.** The catalogue advertises Gemini LLM +
  Assistant + vision + `google_search` web tool, but no
  `fono-llm::gemini` / `fono-assistant::gemini` client exists yet.
  Wizard hides Gemini from the primary picker until the client lands.
- **ElevenLabs TTS.** High-quality voices; intentionally left out of
  the first-wave TTS additions per the plan's "Reality check on TTS
  providers" table. Catalogue-ready slot when a contributor wires it.
- **OpenAI Responses API migration.** OpenAI's `web_search_preview`
  tool will eventually move under the new Responses API surface; the
  catalogue's `WebSearchSupport::NativeTool("web_search_preview")` is
  a stable abstraction that a future client-side migration can absorb
  without touching every consumer.
- **Intent-detection auto-routing for vision/search.** Out of scope —
  the user must explicitly toggle `prefer_vision` /
  `prefer_web_search` in `[assistant]`. A future ADR may revisit
  auto-routing once we have user data on false-positive rates.
- **Screen-capture for vision input.** The catalogue advertises
  multimodal model variants for OpenAI / Anthropic / Groq / Gemini,
  but Fono does not yet capture screen content to attach to assistant
  prompts. `prefer_vision = true` today simply swaps the chat model;
  attaching images is a follow-up.

## Consequences

- Adding a new cloud provider is a single `CloudProvider { … }`
  literal plus one variant in each capability enum the provider
  fills. The compile-time tests catch every consumer that forgot to
  handle it.
- Adding a new capability column (e.g. "image generation") is a one-
  field addition to `CloudProvider`, a runtime check at every
  consumer site, and a row in the matrix in `docs/providers.md`.
- The wizard now collapses a primary-provider pick into one key
  prompt and one capability walk — the user-visible payoff that ADR
  0024 (assistant extras) and issue #11 (multi-provider TTS) both
  ride on.
