# Wizard rework — collapse cloud path onto a single primary provider (issue #9)

## Status: Superseded

GitHub: <https://github.com/bogdanr/fono/issues/9>

## Objective

Reduce the first-run setup to a clearly bounded sequence in which a cloud
user enters **one API key, once**, and walks away with every capability
that key can drive (STT, LLM cleanup, assistant chat, TTS, plus per-key
reuse for the dictation pipeline *and* the optional assistant). Power
users keep an explicit "Customize per capability" escape hatch that
mirrors today's flow. The local path is left structurally intact.

Concretely, the cloud branch must drop from up to **four** API-key
prompts (STT + LLM cleanup + assistant chat + TTS) to **one** for any
provider with broad capability coverage, with the rest auto-filled from
a capability catalogue and validated in a single round trip.

## Initial assessment

### Current shape

| File | Today's behaviour | Pain points |
|---|---|---|
| `crates/fono/src/wizard.rs:46-128` (`run`) | Top-level path picker → Local / Cloud / Mixed → assistant overlay → save | Four sub-flows each ask for a provider + key independently. |
| `crates/fono/src/wizard.rs:487-499` (`configure_cloud`) | `configure_cloud_stt` → `configure_cloud_llm` → `pick_languages` → live-mode prompt | STT and LLM each prompt + validate a key even when one already exists for the same vendor (e.g. picking Groq for both still re-asks "keep `GROQ_API_KEY`?"). |
| `crates/fono/src/wizard.rs:1069-1108` (`configure_cloud_stt`) | 5-way picker: Groq / OpenAI / Deepgram / Cartesia / AssemblyAI | No hint of which providers double-cover LLM/TTS. |
| `crates/fono/src/wizard.rs:1111-1155` (`configure_cloud_llm`) | 5-way picker: Cerebras / Groq / OpenAI / Anthropic / Skip | Same as above — no cross-capability hint. |
| `crates/fono/src/wizard.rs:134-287` (`configure_assistant`) | Independent backend + TTS picker, with OPENAI_API_KEY reuse only | Re-asks the user to pick a backend they may have already covered via the primary provider; reuse logic is OpenAI-only. |
| `crates/fono-core/src/providers.rs:1-477` | Per-backend env-var / parsing tables for STT, LLM, TTS, Assistant | Already the single source of truth for env-var names. **No capability matrix exists yet** — each wizard step hardcodes which providers serve which capability. |
| `crates/fono-core/src/providers.rs:230-243` (`cloud_pair`) | `fono use cloud <name>` knows STT↔LLM pairings (groq, cerebras, openai, anthropic, openrouter, deepgram, assemblyai) | Limited to STT+LLM; ignores TTS and assistant chat. The wizard does not consult this map at all. |
| `docs/providers.md` | Marketing/capability matrix for STT only | LLM / TTS / Assistant capability tables either missing or scattered; future plan output should consolidate. |

### Provider capability matrix (today's runtime support)

| Provider | STT | LLM cleanup | Assistant chat | TTS | Key env var |
|---|---|---|---|---|---|
| **OpenAI**     | ✓ (`whisper-1`)        | ✓ (`gpt-5.4-nano`)   | ✓ (`gpt-5.4-mini`)        | ✓ (`tts-1`) | `OPENAI_API_KEY` |
| **Groq**       | ✓ (`whisper-large-v3-turbo`) | ✓ (`gpt-oss-20b`)    | ✓ (`gpt-oss-120b`)        | ✗ | `GROQ_API_KEY` |
| **Anthropic**  | ✗                      | ✓ (`claude-haiku-4-5`) | ✓ (`claude-haiku-4-5`)    | ✗ | `ANTHROPIC_API_KEY` |
| **Cerebras**   | ✗                      | ✓ (`llama3.1-8b`)    | ✓ (`qwen-3-235b…`)        | ✗ | `CEREBRAS_API_KEY` |
| **OpenRouter** | ✗                      | ✓                     | ✓                          | ✗ | `OPENROUTER_API_KEY` |
| **Gemini**     | ✗                      | ✓                     | ✓                          | ✗ | `GEMINI_API_KEY` |
| **Deepgram**   | ✓ (`nova-2`)           | ✗                     | ✗                          | ✗ | `DEEPGRAM_API_KEY` |
| **AssemblyAI** | ✓ (`best`)             | ✗                     | ✗                          | ✗ | `ASSEMBLYAI_API_KEY` |
| **Cartesia**   | ✓ (`sonic-transcribe`) | ✗                     | ✗                          | (Wyoming-side only today; TTS API exists but not wired) | `CARTESIA_API_KEY` |

OpenAI is the only **full-stack** cloud provider. Groq covers three of
four. Cartesia is a future second full-stack candidate once TTS is
wired; Plan leaves that as a follow-up rather than a blocker.

### Findings, ranked by user impact

1. **Cloud key prompts are not deduplicated across capabilities.** A
   user who chose Groq STT, then Groq LLM, then Groq assistant gets
   asked three times whether to keep `GROQ_API_KEY`. Highest-impact,
   lowest-risk fix. (`crates/fono/src/wizard.rs:1093-1095, 1144-1146,
   210-214`.)
2. **Capability coverage is invisible to the user.** Nothing on the
   STT picker hints that picking Groq also covers LLM cleanup and the
   assistant. Users default to a multi-provider setup not by choice
   but by ignorance. (`crates/fono/src/wizard.rs:1074-1080,
   1116-1122`.)
3. **No capability catalogue exists.** Adding one in
   `fono-core` lets the wizard, `fono use cloud <name>`, and
   `fono doctor` share the same source of truth.
4. **"Mixed" is a confusing label.** It currently means
   "pick STT and LLM independently" but says nothing about the
   assistant or TTS. A clearer "Customize per capability" or
   "Advanced — pick a different provider for each step" makes it the
   right escape hatch for the new flow.
5. **Assistant + TTS are unconditionally a second mini-wizard.**
   Today even a Cloud → OpenAI user is forced through `configure_assistant`'s
   provider menu, despite OpenAI already covering chat + TTS. Should
   collapse to a single yes/no when the primary already covers
   everything.

### Key design decisions / assumptions

- **Single primary cloud provider as the new default.** A
  `PathChoice::Cloud` branch becomes "pick one primary provider; fill
  any gaps with secondary providers if you want to." `Mixed` becomes
  the renamed advanced path, identical to today's behaviour except
  shown with key-reuse short-circuits.
- **Capability catalogue lives in `fono-core`**, next to
  `providers.rs`, so it can ship with the binary, be diffable, and
  drive both the wizard and `fono doctor`'s upcoming "is this key
  worth typing" hints.
- **The catalogue only describes provider × capability mappings**,
  not the runtime factories. Concrete model strings continue to live
  in `fono-stt::defaults`, `fono-llm::defaults`, and
  `fono-tts::defaults`. The catalogue references those defaults by
  symbol or by re-export, not by hard-coding model names a second
  time.
- **Validation is consolidated.** A single `validate_cloud_key` call
  (re-using `crates/fono/src/wizard.rs:1200-1247`) per primary
  provider gates **all** capabilities derived from it. Failure
  prompts the user once for "save anyway" / "retry" / "switch
  provider", not once per capability.
- **No new dependencies.** All work is rearrangement + catalogue +
  prompts.

## Implementation Plan

### Phase A — Capability catalogue (foundation)

- [ ] Task A1. Add `crates/fono-core/src/provider_catalog.rs` exposing
      a `CloudProvider` struct (`id: &str`, `display_name: &str`,
      `tagline: &str`, `console_url: &str`,
      `stt: Option<SttDefaults>`, `llm: Option<LlmDefaults>`,
      `assistant: Option<AssistantDefaults>`,
      `tts: Option<TtsDefaults>`, `key_env: &str`) and a `const`
      table `CLOUD_PROVIDERS: &[CloudProvider]`. Each `*Defaults`
      sub-struct carries the canonical model identifier and any
      voice / latency hint shown in the wizard.
      Rationale: a typed, exhaustive catalogue lets the wizard
      produce its rows without `match` statements, and lets future
      capabilities (e.g. embeddings, image input) extend the struct
      without touching call sites.
- [ ] Task A2. Re-export the catalogue from `fono_core::lib.rs`,
      mirroring how `providers` is exposed; add it to the public
      surface so `crates/fono` can import it directly and
      `crates/fono-tray` can reuse the same metadata for its
      submenu labels later.
- [ ] Task A3. Move/refactor `cloud_pair` to read from the
      catalogue: `cloud_pair(id)` should return the `(SttBackend,
      LlmBackend)` of the catalogue entry whose `id` matches,
      preserving today's behaviour as a thin wrapper. Add a
      regression test that every entry of the legacy hand-coded
      pair list still resolves identically.
      Rationale: ensures `fono use cloud <name>` and the wizard
      agree on what each provider covers.
- [ ] Task A4. Unit tests: every catalogue entry's `key_env`
      matches the canonical env var returned by
      `providers::*_key_env` for each backend it claims to support;
      providers that claim a capability also have a matching
      `Backend` enum variant; round-trip lower-case `id` parses
      with `parse_*_backend`.

### Phase B — Wizard cloud-path collapse

- [ ] Task B1. Introduce a `pick_primary_cloud_provider` helper in
      `crates/fono/src/wizard.rs` that renders the catalogue as a
      `Select` with two-line labels (provider name + capability
      badges, e.g. `"OpenAI — STT · LLM · Assistant · TTS"`,
      `"Groq — STT · LLM · Assistant (fastest cloud STT)"`,
      `"Customize per capability (advanced)"`). Default cursor on
      OpenAI (broadest coverage) unless an existing key already
      sits in `secrets.toml` for another provider, in which case
      default to that provider's row to make re-runs cheap.
- [ ] Task B2. Replace `configure_cloud` so the cloud branch calls
      `pick_primary_cloud_provider`, runs **one**
      `prompt_api_key_with_validation` against the chosen
      provider's `key_env`, then walks the catalogue entry and
      fills each capability with its default. Capabilities the
      primary can't serve (e.g. TTS on Groq) get a follow-up
      "Add <capability>? — yes / skip / pick from <secondary
      list>" prompt that only enumerates providers actually
      offering that capability, again with key-reuse short-circuit.
      Live-mode + language pickers run unchanged at the end of
      the branch.
- [ ] Task B3. Update `configure_assistant` to consult the
      catalogue and the primary-provider choice. When the primary
      already covers the assistant *and* either covers TTS or has
      been paired with a TTS provider in B2, the assistant prompt
      collapses to a single yes/no ("Enable the voice assistant
      with <primary> chat + <tts_choice> TTS?"). If the user
      declines, fall through to today's full picker for those who
      want a separate stack. Existing
      "Anthropic recommended for assistant" copy is preserved
      only when the user explicitly enters the Customize path.
- [ ] Task B4. Rename `PathChoice::Mixed` to a new variant
      (e.g. `PathChoice::Customize`) and update its menu label to
      "Customize each capability (advanced)". Old branch body is
      retained verbatim — this is purely a relabel so the new
      cloud path doesn't compete with itself for the "I want
      different providers" use case. Catalogue-aware key-reuse
      messaging is still applied here so the Customize branch
      also stops re-prompting for the same env var.
- [ ] Task B5. Centralise key-reuse logic into a single
      `prompt_or_reuse_key` helper that takes a `key_env` and a
      catalogue entry, and prints a one-line summary
      (`"reusing OPENAI_API_KEY from secrets.toml"` or
      `"validating new OPENAI_API_KEY …"`) before delegating to
      `prompt_api_key_with_validation`. Every cloud key prompt in
      the file routes through it. Rationale: eliminates the
      duplicate "keep existing key?" prompt cluster that today
      makes re-running the wizard feel hostile.

### Phase C — Documentation & user-facing copy

- [ ] Task C1. Update `docs/providers.md` to lead with the
      capability matrix (the table reproduced above), followed by
      the per-capability sub-sections that exist today. Cite the
      catalogue as the source of truth and link to
      `provider_catalog.rs`.
- [ ] Task C2. Refresh `README.md`'s "Switching providers"
      paragraph so the quoted wizard output matches the new
      single-prompt cloud branch and call out that one key now
      covers multiple capabilities for the broad-coverage
      providers.
- [ ] Task C3. Add a `CHANGELOG.md` `[Unreleased]` entry under
      `### Changed`: the wizard cloud path now asks for one
      primary provider + one key by default; explain how the
      Customize escape hatch works; mention key reuse is now
      provider-agnostic. (`docs/status.md` log entry to be
      written when the work lands, per AGENTS.md cadence.)

### Phase D — Tests, smoke, and ADR

- [ ] Task D1. Add `crates/fono/tests/wizard_flow.rs` style
      integration tests (or extend existing) that drive the new
      cloud branch via the pure helpers (no TTY): assert that
      picking "OpenAI" sets `stt.backend = OpenAI`,
      `llm.backend = OpenAI`, `assistant.backend = OpenAI`,
      `tts.backend = OpenAI`, and writes exactly one key entry.
      Assert that picking "Groq" sets STT + LLM + assistant to
      Groq, leaves `tts.backend = None` (unless a follow-up was
      taken), and writes one key entry. Run the same flow twice
      to confirm the second pass reuses the saved key without a
      second prompt.
- [ ] Task D2. Add a unit test that the Customize branch
      preserves today's behaviour bit-for-bit when each capability
      picks a different provider (regression guard for power
      users).
- [ ] Task D3. Manual smoke checklist appended to
      `plans/2026-05-04-fono-prelaunch-ux-polish-and-smoke-tests-v1.md`
      (already covers wizard runs): add three scenarios — fresh
      `~/.config/fono` with OpenAI key only; pre-existing Groq
      key + wizard re-run; Customize branch with mixed Groq STT +
      Anthropic LLM + Wyoming TTS.
- [ ] Task D4. ADR `docs/decisions/0006-cloud-provider-catalogue.md`
      capturing the decision to centralise capability metadata in
      `fono-core::provider_catalog`, the alternatives considered,
      and the deferred-work list (Cartesia full-stack, Gemini STT
      once supported).

## Verification Criteria

- A fresh wizard run on the cloud branch with no existing secrets
  asks for **at most one** API key when the user picks any
  full-stack provider (OpenAI today), and at most two when the user
  picks a near-full-stack provider plus an opt-in TTS (e.g. Groq +
  Wyoming).
- Re-running the wizard with `~/.config/fono/secrets.toml` already
  populated never re-asks for a stored key; the prompt is replaced
  by a one-line "reusing …" notice.
- `cargo test -p fono-core -p fono` is green; new tests added for
  the catalogue, the cloud-branch helpers, and the Customize
  regression guard.
- `fono doctor` and `fono use cloud <name>` resolve every
  catalogue entry — no orphan provider id, no missing env-var
  mapping (asserted by Task A4 + Task A3 regression test).
- `docs/providers.md`, `README.md`, and `CHANGELOG.md` updates
  match the shipped wizard wording (verified by the smoke
  checklist in `plans/2026-05-04-…-prelaunch-ux-polish-…-v1.md`).
- `cargo clippy --workspace --all-targets -- -D warnings` is
  clean; no new dependencies added (verified by `cargo deny check`
  exit status).

## Potential Risks and Mitigations

1. **Catalogue drift versus runtime factories.** Default model
   strings live in `fono-stt::defaults`, `fono-llm::defaults`, and
   `fono-tts::defaults`. The new catalogue duplicates them by
   value.
   Mitigation: have the catalogue `pub use` the constants from the
   respective `defaults` modules rather than hard-coding string
   literals, and add a compile-time assertion (Task A4) that the
   catalogue references resolve.
2. **Users who genuinely want a multi-provider stack feel
   railroaded.** The new default-collapse path may obscure the
   Customize escape.
   Mitigation: Customize is a sibling menu entry on the primary
   picker (not buried under "Advanced…"), with its own
   one-line description; smoke scenario D3 exercises it
   explicitly.
3. **OpenAI-only TTS today means Groq + Anthropic + Cerebras users
   either skip TTS or accept a second API key.** This is the
   correct shape but may surprise some users.
   Mitigation: surface this trade-off in the primary picker
   description ("TTS not available — assistant runs text-only or
   adds OpenAI TTS as a follow-up") and document it in
   `docs/providers.md`.
4. **`fono use cloud <name>` and the new wizard must stay in
   lockstep.** Diverging "Groq covers what?" answers across
   commands erode trust.
   Mitigation: Task A3 routes `cloud_pair` through the catalogue
   so both surfaces read the same data; regression test asserts
   the old hand-coded pair list is preserved.
5. **Re-running the wizard after this change must remain
   non-destructive for existing v0.7.x users.** A v0.7.x config
   already on disk should round-trip through the new flow without
   silently flipping `tts.backend` from `Wyoming` to `OpenAI`.
   Mitigation: the wizard already starts from `Config::default()`
   (`crates/fono/src/wizard.rs:73`), but should be amended to
   pre-fill defaults from the existing config when present, so the
   primary picker pre-selects the user's current provider rather
   than the catalogue default. Add a unit test that round-trips
   a v0.7.1-shaped config through the new wizard helpers.
6. **Catalogue display order influences first-time-user
   conversion.** Ordering OpenAI first defaults users into a key
   they may not have; ordering Groq first defaults them into a
   provider with no TTS.
   Mitigation: order by capability count then by "broadest
   typical use case" (OpenAI → Groq → Anthropic → Cerebras → …).
   Surface the existing-key default so re-runs prefer whatever
   the user already pays for.

## Alternative Approaches

1. **Tabular full-screen picker.** A single TUI screen with
   providers as rows and capabilities as columns lets the user tick
   each cell. Pros: every trade-off is visible at once; teaches the
   matrix. Cons: requires a richer terminal widget than
   `dialoguer::Select`; harder on narrow terminals and screen
   readers; out of scope for the dialoguer-only stack.
2. **Two-step minimalist quick-start.** Replace the entire cloud
   branch with a two-question flow: "What's your API key?" → detect
   provider from key prefix (`sk-` → OpenAI, `gsk-` → Groq, …) →
   auto-fill everything. Pros: shortest possible flow. Cons: key
   prefixes are not load-bearing identifiers (Anthropic and several
   OpenAI variants share `sk-…`); guessing wrong is hostile;
   doesn't help re-runs.
3. **Defer everything to `fono settings`.** Have the wizard
   write only a hardware-tier-appropriate default config and let
   users run `fono settings` afterwards. Pros: zero prompts. Cons:
   regresses the issue (no cloud key prompt at all means users
   never get the dictation pipeline working on cloud); contradicts
   the issue's "ask for one key" framing.
4. **Catalogue-driven approach** (this plan). Pros: smallest
   surface change; preserves Customize; teachable; aligns with the
   existing `providers.rs` module's role as the single source of
   truth. Cons: introduces a new module and a new unit-test
   surface. Trade-off accepted.
