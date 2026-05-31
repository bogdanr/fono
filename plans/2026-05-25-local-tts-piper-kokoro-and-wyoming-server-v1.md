# Local TTS (Piper + Kokoro) and Fono-as-a-Wyoming-TTS-Server

> **SUPERSEDED (2026-05-31)** by
> `plans/2026-05-31-local-tts-ggml-piper-kokoro-and-wyoming-server-v2.md`.
> v2 replaces this plan's "third `fono-tts` variant" + ONNX-fallback
> strategy with **ggml-reuse** (no new variant; TTS absorbed into the
> CPU + Vulkan builds), corrects Piper's license (now GPL-3.0,
> `OHF-Voice/piper1-gpl`), pulls the Wyoming server forward as the first
> code slice, and gates Kokoro behind an explicit feasibility spike.
> This file is kept for history; do not execute it.

## Objective

Make Fono speak — locally, multilingually, including Romanian — in the
same single-binary spirit as today's local STT (whisper.cpp) and local
LLM (llama.cpp) stories. Then expose that local TTS engine on the
network so Home Assistant (and any other Wyoming client) auto-discovers
Fono as a Wyoming-protocol TTS service, replacing the separate
`wyoming-piper` Python sidecar that HA Voice deployments rely on today.

Two engines, one router:

- **Kokoro** (Apache-2.0) when the requested language is one of its
  nine trained locales (American / British English, Spanish, French,
  Hindi, Italian, Japanese, Brazilian Portuguese, Mandarin). Better
  prosody, smaller weights, lower latency.
- **Piper** (MIT) for everything else, including **Romanian**
  (`ro_RO-mihai-medium`) and the long tail of European / Slavic /
  Asian languages that Kokoro v1.0 does not cover.

The router design already exists in
`plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md:111-123`; this
plan promotes Kokoro from a future tracker into an active phase and
adds the Piper fallback branch + the network-server endpoint.

## Background — what already ships

The plumbing for this work is mostly in place. Auditing the tree
2026-05-25:

- **Wyoming-protocol codec** (events: info, describe, transcribe,
  synthesize, audio-start, audio-chunk, audio-stop) —
  `crates/fono-net-codec/src/wyoming.rs`. The synthesize and audio-*
  events are already decoded by today's *client* in
  `crates/fono-tts/src/wyoming.rs:80-260`.
- **Wyoming server**, accept loop, hot-reload via `Arc<dyn …>`
  provider closure — `crates/fono-net/src/wyoming/server.rs:1-475`.
  Currently STT-only; the connection handler at `:261-360` dispatches
  `Describe` / `Transcribe`, and `build_info` at `:443-473` advertises
  only the `asr: vec![…]` branch.
- **mDNS advertiser and browser** for `_wyoming._tcp.local.` and
  `_fono._tcp.local.` — `crates/fono-net/src/discovery/{advertiser,
  browser, mod, txt}.rs`. The TXT schema at `discovery/txt.rs:8-16`
  already includes a comma-separated `caps` key; adding `"tts"` to
  the tag list is purely additive.
- **`[server.wyoming]` config block** + headless install
  (`fono install --server`, hardened systemd unit) — already shipping
  per v0.4.0 and v0.6.1 release notes in `ROADMAP.md`.
- **Audio resampling and playback** — `rubato` (`Cargo.toml:77`),
  `cpal` (`Cargo.toml:76`).
- **Model-download / cache infrastructure** — reused from the Whisper
  model fetcher; lays down files under `~/.cache/fono/models/…`.
- **GGML backend, OpenMP, static libstdc++** — already linked once via
  whisper.cpp + llama.cpp (`Cargo.toml:79-93`). Piper-GGUF and
  Kokoro-GGUF ports can re-use the existing ggml symbols if we expose
  them across the workspace (see Risks §2).

What is **missing**:

1. In-process Piper inference engine.
2. In-process Kokoro inference engine + voice router.
3. `libespeak-ng` bundled and pointed at a lazy-downloaded data dir
   under `~/.cache/fono/espeak-ng-data/`.
4. `Info.tts` branch and a `Synthesize` arm in the Wyoming server's
   connection handler.
5. `caps=tts` in the mDNS TXT advertisement and a `[server.tts]`
   config block.

## Pinned decisions (carried over from chat, 2026-05-25)

| Decision | Choice |
|---|---|
| Romanian support | **Required**. Drives Piper inclusion. |
| Engine selection per language | **Kokoro-primary, Piper-fallback** via the router. |
| `libespeak-ng` | **Bundled statically.** ~3 MB code; no usable pure-Rust multilingual phonemizer exists today. |
| espeak-ng language data | **Downloaded at runtime per configured language**, just like Whisper models. ~100–500 KB per language under `~/.cache/fono/espeak-ng-data/`. |
| Piper voices + Kokoro weights / styles | **Downloaded at runtime**, never bundled. |
| Phase ordering | **Engines first, server endpoint last.** Phase 1 = Piper. Phase 2 = Kokoro + router. Phase 3 = Wyoming TTS server + HA discovery. |
| Size budget for the local-TTS-enabled artefact | **New variant** (ADR 0022 amendment required), analogous to the existing `fono-gpu-*` variant at +42 MB. Realistic cap **≤ 32 MiB** for the local-TTS variant; canonical `fono` stays at 20 MiB. |

## Architecture: a third release variant

Following the precedent set by ADR 0022's 2026-05-02 amendment that
introduced the GPU variant: local TTS ships as a **third release
asset**, not as bytes added to the canonical ship binary.

- `fono-vX.Y.Z-x86_64` — current 20 MiB CPU build, unchanged.
- `fono-gpu-vX.Y.Z-x86_64` — current Vulkan build, unchanged.
- `fono-tts-vX.Y.Z-x86_64` (**new**) — CPU + local TTS engines + bundled
  `libespeak-ng`, target **≤ 32 MiB**.

`fono update` already picks variants at runtime (`crates/fono/src/...`
update path, ROADMAP entry under v0.5.0). The local-TTS variant is
auto-selected when `[tts].backend = "local"` and the binary lacks the
local engines — wizard offers a one-click upgrade, same UX as the GPU
variant.

Why a variant rather than absorbing into the main binary:

- Honours ADR 0022's 20 MiB invariant for headless / cloud-only users.
- Mirrors the GPU pattern that users already understand.
- Lets us optimise the local-TTS variant aggressively (different
  link flags, different LTO settings) without affecting the canonical
  build.
- Cloud-only users (the majority on day one) pay zero bytes for
  features they don't use.

ADR 0022 is amended in Task 0 (below).

## Implementation Plan

### Task 0 — ADR amendment

- [ ] Amend `docs/decisions/0022-binary-size-budget.md` with a third
  variant `tts` (≤ 32 MiB, NEEDED ⊆ `{libc, libm, libgcc_s,
  ld-linux}`; `libespeak-ng` static, no new dynamic deps). Update the
  CI size-budget matrix. Update `tests/check.sh --size-budget` to
  carry the new row.

### Phase 1 — Piper in-process (the heavy lift)

- [ ] **1.1** Add a `tts-local` feature gate to `crates/fono-tts`
  (off by default; on for the `fono-tts` variant). Behind it pull in
  the Piper engine crate (either upstream `piper-rs` or a vendored
  fork) and a `libespeak-ng-sys` binding with `static = true` build.
- [ ] **1.2** Bundle `libespeak-ng` statically. The C build needs the
  same `-Os -ffunction-sections -fdata-sections` + `--gc-sections`
  treatment whisper.cpp / llama.cpp already get (per ADR 0022 Phase 1
  Task 1.3). espeak-ng's data dir is *not* compiled in; we point the
  runtime at `~/.cache/fono/espeak-ng-data/` via
  `espeak_ng_InitializePath`.
- [ ] **1.3** Lazy espeak-ng-data downloader. Mirror the whisper-model
  fetcher pattern: catalogue the per-language `.dict` files +
  shared `phontab`/`phonindex` from upstream espeak-ng releases,
  pin SHA-256s, download on first use for each language in
  `general.languages`. Test by uninstalling the cache dir and
  starting cold.
- [ ] **1.4** Piper voice catalogue + downloader. Voices live at
  `rhasspy/piper-voices` on HuggingFace; ship a curated subset
  (one per supported locale) with metadata in
  `crates/fono-tts/src/piper_voices.rs`. Cache under
  `~/.cache/fono/models/piper/`. SHA-256-pinned.
- [ ] **1.5** `PiperTts` struct implementing the existing
  `TextToSpeech` trait (`crates/fono-tts/src/traits.rs`). Streaming
  chunked synthesis: feed espeak-ng the input text, get phoneme IDs,
  run them through the VITS+HiFi-GAN graph, emit f32 PCM in
  ~80 ms chunks. First-token latency target: **<300 ms on a 4-core
  x86 CPU** (Piper is a bit slower than the Kokoro target).
- [ ] **1.6** Factory wire-up + `[tts.local]` config block. Default
  voice resolution: pick the first matching voice from the catalogue
  for the user's configured language. Honour explicit `voice =`
  overrides.
- [ ] **1.7** Wizard integration. Local TTS becomes a picker option
  alongside Wyoming and the cloud backends; selecting it triggers
  the espeak-ng-data + Piper-voice downloads with a progress bar
  identical to the Whisper model download.
- [ ] **1.8** Doctor + tray exposure. `fono doctor` reports the local
  engine status, voice cache size, espeak-ng-data version. Tray
  "TTS backend" submenu lists the local engine when the variant
  supports it.
- [ ] **1.9** Tests: unit tests for the espeak-ng-data downloader
  cache layout, voice-catalogue resolver, factory round-trip; a
  smoke test that synthesises a known Romanian and English phrase
  and checks output PCM length / non-silence.
- [ ] **1.10** Variant build + CI size-budget gate. Add the
  `fono-tts-x86_64` matrix entry in `release.yml`; assert the
  artefact is ≤ 32 MiB and `NEEDED` is the canonical four-entry
  allowlist.
- [ ] **1.11** Docs + changelog + roadmap promotion entry for the
  release that ships Phase 1.

### Phase 2 — Kokoro in-process + voice router

- [ ] **2.1** Add Kokoro inference module to `crates/fono-tts` under
  the same `tts-local` feature gate. Use a GGUF / ggml-backed
  Kokoro port if one is mature by build time; otherwise the ONNX
  Runtime route from
  `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md:19-24`
  becomes the fallback at a known size cost (+~12 MB; would push
  the variant past 32 MiB and require a second ADR amendment).
  **Decision deferred to Phase 2 kickoff** based on the state of
  the GGML/ONNX port landscape at that time.
- [ ] **2.2** Kokoro voice + weight catalogue. The 54-voice / 9-locale
  table from
  `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md:30-40` and
  `:111-123`. Voices ~10 MB total, weights ~80–330 MB depending on
  quantization. SHA-256-pinned.
- [ ] **2.3** misaki-equivalent G2P (or its Rust port if one exists).
  For Phase 2, this is the only languages-bundled piece — the 9
  Kokoro locales' phoneme rules are baked in (~300–600 KB). Piper
  continues to use espeak-ng for its long tail.
- [ ] **2.4** `KokoroVoiceRouter` from the cross-linked plan:
  `pick_voice(BCP-47 lang) -> Option<&'static str>`. `None` →
  fall through to Piper. Promote the cross-linked plan to **closed,
  superseded by this one** when Phase 2 lands.
- [ ] **2.5** Engine dispatcher in `factory.rs`. When `[tts].backend
  = "local"`, build a `LocalTtsRouter` that wraps both `KokoroTts`
  and `PiperTts` and routes per request based on the
  caller-supplied `lang`. Caller (the assistant pipeline,
  `crates/fono/src/session.rs`) already passes a language hint.
- [ ] **2.6** Wizard updates: unified "Local" picker; Kokoro-vs-Piper
  is a visible but secondary choice ("auto by language" is the
  default).
- [ ] **2.7** Tests: router unit tests for every supported language
  including Romanian (→ Piper), French (→ Kokoro), English (→
  Kokoro), Polish (→ Piper). Round-trip smoke test in two languages.
- [ ] **2.8** Size-budget recheck. If Phase 2 pushes the variant
  past 32 MiB, amend ADR 0022 a second time (or split into a
  separate `fono-tts-pro` variant — undesirable but possible).
- [ ] **2.9** Docs / changelog / roadmap promotion for the release
  that ships Phase 2.

### Phase 3 — Fono as a Wyoming TTS server, autodiscoverable by HA

This is the small phase. ~150 lines of code on top of the existing
Wyoming STT server.

- [ ] **3.1** Extend `Info` in `fono-net-codec` (if not already) to
  carry the `tts: Vec<TtsProgram>` field with `name`, `voices: Vec<TtsVoice>`,
  `attribution`, `installed`, `version`, `description`,
  `supports_synthesize_streaming`. Mirror the existing `AsrProgram`
  shape used at `crates/fono-net/src/wyoming/server.rs:441-474`.
- [ ] **3.2** Add a `TtsProvider = Arc<dyn Fn() -> Arc<dyn
  TextToSpeech>>` to `WyomingServer` parallel to the existing
  `SttProvider` at `:153-160`. Hot-reload semantics identical.
- [ ] **3.3** `Synthesize` event handler in `handle_connection`
  (`server.rs:261-360`). Read `{ text, voice, language }`, invoke
  `tts.synthesize(...)`, emit `audio-start` (with rate / width /
  channels) → `audio-chunk*` → `audio-stop`. Streaming chunked
  output so HA pipelines first audio while later chunks render.
- [ ] **3.4** Update `build_info` (`server.rs:443-473`) to populate
  the `tts` field whenever the `TtsProvider` is bound. ASR and TTS
  can coexist on the same listener (Wyoming protocol multiplexes
  by event type).
- [ ] **3.5** `[server.tts]` config block in `fono-core/src/config.rs`:
  `enabled`, `voices` (the subset of the local catalogue to advertise),
  `default_voice`. Loaded by `fono install --server` exactly like
  `[server.wyoming]`.
- [ ] **3.6** mDNS TXT update. Add `"tts"` to the `caps` tag list in
  `crates/fono-net/src/discovery/advertiser.rs` when `[server.tts]`
  is enabled. The schema at `discovery/txt.rs:12` already supports
  this purely additively — `parse_caps` (`:35-37`) tolerates
  arbitrary tags. Add a round-trip test.
- [ ] **3.7** Home Assistant verification. With Fono running with
  `[server.tts].enabled = true`, HA's Settings → Devices & services
  → Add integration → Wyoming Protocol → Discovered shows Fono as
  a TTS-capable service; selecting it registers a usable
  `tts.fono` entity. Manual test on a real HA box; document the
  flow in `docs/providers.md`.
- [ ] **3.8** Tests:
  `crates/fono-net/tests/wyoming_server_round_trip.rs` gains a
  synthesize round-trip case;
  `crates/fono-net/tests/discovery_round_trip.rs` gains a `caps`
  containing `tts` assertion.
- [ ] **3.9** Docs / changelog / roadmap promotion for the release
  that ships Phase 3.

## Verification criteria

### Phase 1

- `fono-tts-x86_64` artefact ≤ 32 MiB; `NEEDED` matches the
  four-entry allowlist; CI size-budget matrix passes.
- Fresh install on a machine with no cache dir: selecting local
  TTS downloads espeak-ng-data + one Piper voice and synthesises
  the first sentence within 30 s on a 4-core x86 CPU.
- Romanian dictation reply (`general.languages = ["ro"]`) reads
  back in `ro_RO-mihai-medium`.
- First-token latency p50 < 300 ms on a 4-core x86 CPU for a
  20-word English utterance.

### Phase 2

- A user with `[general].languages = ["en", "ro", "fr"]` hears:
  Kokoro for `en` and `fr` utterances, Piper for `ro`. Switching
  is automatic per assistant reply, no config change.
- All 54 Kokoro voices addressable; all curated Piper voices
  addressable. Wizard customise step lists both.
- Variant size still ≤ 32 MiB *or* ADR 0022 re-amended with the new
  cap explicitly documented.

### Phase 3

- A vanilla Home Assistant install on the same LAN detects Fono
  under "Wyoming Protocol" within 30 s of `[server.tts].enabled
  = true` taking effect.
- HA's `tts.speak` action targeting the Fono entity returns audio
  whose Romanian utterance matches the local `fono speak` output
  byte-for-byte (modulo network framing).
- `caps=tts` is present in the mDNS TXT record published by Fono
  and parsed correctly by Fono's own browser (round-trip test).
- ASR and TTS can be served simultaneously from the same Fono
  daemon on the same port.

## Out of scope

- Voice cloning, custom voices, fine-tunes (carried over from
  `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md:71-75`).
- WebSocket streaming TTS (the Wyoming protocol's audio-chunk
  stream is sufficient for HA's needs).
- Wyoming wake-word service (separate project; see ROADMAP
  "Wake-word activation").
- GPU acceleration for TTS engines (CPU-only in this scope;
  revisit if first-token latency targets miss).
- macOS / Windows builds of the `fono-tts` variant (Linux first;
  cross-platform follow-up once Phase 1 has soaked).

## Open questions

1. **GGML port maturity for Piper and Kokoro at Phase 1 / 2 start.**
   If a clean GGUF / ggml-backed Piper port exists and integrates
   with the workspace ggml build, we save ~12 MB by reusing
   whisper.cpp / llama.cpp's ggml. If not, ONNX Runtime is the
   fallback at known size cost. Decision deferred until phase
   kickoff.
2. **Should the `tts-local` feature be exposed on the canonical
   `fono` binary as opt-in source builds?** Probably yes — users who
   build from source with `--features tts-local` get the engines
   without needing the variant artefact. CI gate covers only the
   variant.
3. **Per-language voice override schema** (`Option<HashMap<lang,
   voice>>`) deferred to a follow-up; the simple "first matching
   voice in catalogue" rule covers Phase 1 / 2.

## Risks and mitigations

1. **Phonemizer code size is irreducible.** Documented in chat
   2026-05-25. Mitigation: variant binary at 32 MiB; bundle
   `libespeak-ng` once, share between Piper and (post-Phase 2,
   long-tail languages of) Kokoro.
2. **Multiple ggml copies blow up binary size.** Risk if Piper /
   Kokoro GGUF ports vendor their own ggml. Mitigation: same
   source-shared-ggml technique as ADR 0022 Phase 1 Task 1.2;
   alternatively fall back to ONNX Runtime if shared-ggml proves
   too invasive (with the documented size cost).
3. **espeak-ng data file SHA churn upstream.** Espeak-ng releases
   regenerate compiled `.dict` files. Mitigation: pin SHAs per
   upstream release; refresh on each espeak-ng version bump as
   part of the dependency rotation.
4. **Piper voice quality variance.** Some languages in
   `rhasspy/piper-voices` have only "low" quality voices.
   Mitigation: curate the bundled catalogue to medium-or-better
   per locale; surface grades in the wizard customise step.
5. **Home Assistant Wyoming integration drift.** HA's `tts.speak`
   contract could change across releases. Mitigation: Phase 3
   manual verification step against the current stable HA
   release; subscribe to the HA Wyoming integration repo for
   breaking-change notifications.
6. **First-token latency budget miss.** If Piper or Kokoro can't
   hit <300 ms / <200 ms p50 on a 4-core x86 CPU, the assistant
   pipeline gains a perceptible pause. Mitigation: streaming
   chunked synthesis (already designed); cap pre-roll buffer at
   one sentence; fall back to "render-then-play" mode on slow
   hosts with a one-time warning.

## Cross-links

- `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md` — local
  Kokoro design + voice router. This plan **subsumes its Phase 1**
  (local backend, router); the cloud-parity portion (OpenRouter
  passthrough using the same router) remains a separate concern
  and stays open in that plan until the local engine lands here.
- `plans/closed/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`
  — the foundation Phase 3 builds on.
- `plans/closed/2026-05-22-fono-server-install-auto-listen-v1.md` —
  the `fono install --server` ergonomics this work inherits.
- `docs/decisions/0022-binary-size-budget.md` — amended by Task 0.
- `docs/decisions/0004-default-models.md` — both engines are OSI
  Apache-2.0 / MIT, so both qualify as default-eligible.

## Alternative approaches

1. **Sidecar binary instead of variant.** Ship a separate
   `fono-tts-server` binary (downloaded like a model) that speaks
   Wyoming on localhost; the main `fono` process uses today's Wyoming
   TTS client (`crates/fono-tts/src/wyoming.rs`) to drive it. Keeps
   the main binary at 18 MiB. Rejected for v1 because it
   complicates the install / update story (two binaries to keep in
   sync, two systemd units, surprise on `fono install --server`).
   Worth reconsidering if the variant pattern itself becomes
   unwieldy in maintenance.
2. **WASM-compiled engines lazy-downloaded.** Compile Piper and
   Kokoro to WASM, ship a tiny `wasmi` runtime (~300 KB), download
   `.wasm` modules on first use. Hits the ≤1 MB main-binary growth
   target. Rejected for v1 because no production-grade WASM port
   of either engine exists today; we'd be doing the port ourselves
   with unknown latency cost. Recapture if the variant binary
   becomes a release-blocker.
3. **espeak-ng as a system dependency.** Declare `REQUIRES=espeak-ng`
   in the SlackBuild and equivalent on other distros. Saves ~3 MB
   from the variant. Rejected because it contradicts the
   single-binary promise; documented for completeness.
4. **Skip Phase 3 (Wyoming TTS server endpoint).** Just ship local
   TTS for the user's own dictation. Rejected because the
   incremental cost is ~150 lines on top of infrastructure that
   already ships, and the HA story is a strong strategic win for
   the project beyond Fono's primary user.
