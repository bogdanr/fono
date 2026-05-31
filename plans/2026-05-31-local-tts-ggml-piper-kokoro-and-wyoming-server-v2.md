# Local TTS (ggml: Piper + Kokoro) and Fono-as-a-Wyoming-TTS-Server — v2

> **SUPERSEDED 2026-05-31 by
> `plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`.**
> This plan's **ggml-reuse** substrate was reversed once the project
> committed to a full local voice stack: per ADR 0032 the stack now runs
> on **statically-linked ONNX Runtime** (`ort`), built minimally, with
> shared-ggml as a later size-offset rather than a prerequisite. This
> file is retained for its **evidence trail** — the Piper-ggml micro-spike
> and the static-ONNX spike results appended near the end remain the
> authoritative record of *why* the substrate changed. Read v3 for the
> current plan.

> **Supersedes** `plans/2026-05-25-local-tts-piper-kokoro-and-wyoming-server-v1.md`.
> v1 was drafted before two facts changed the design: (a) Piper was
> relicensed GPL-3.0 (`OHF-Voice/piper1-gpl`, archived `rhasspy/piper`),
> and (b) Fono will **never** ship a third release variant — local TTS
> is absorbed into the existing **CPU** and **Vulkan** builds. This v2
> replaces v1's "third `fono-tts` variant" + ONNX-fallback strategy with
> a **ggml-reuse** strategy that keeps the canonical binary small, and
> moves the engine-feasibility question for Kokoro to an explicit spike
> placed just before the Kokoro implementation.

## Objective

Make Fono speak — locally, multilingually, including Romanian — in the
same single-binary spirit as today's local STT (whisper.cpp) and local
LLM (llama.cpp). Then expose that local engine on the network so Home
Assistant (and any Wyoming client) auto-discovers Fono as a
Wyoming-protocol TTS service, replacing the `wyoming-piper` Python
sidecar HA Voice deployments rely on today.

Two engines, one router (unchanged from v1's intent):

- **Kokoro** (Apache-2.0) for its trained locales (American / British
  English, Spanish, French, Hindi, Italian, Japanese, Brazilian
  Portuguese, Mandarin) — best prosody, small weights, low latency.
- **Piper** (**GPL-3.0**, `OHF-Voice/piper1-gpl`) for everything else,
  including **Romanian** (`ro_RO-mihai-medium`) and the long tail of
  European / Slavic / Asian languages Kokoro v1.0 does not cover.

## Why this is a v2 — the decisions that changed

| Topic | v1 | v2 (this plan) |
|---|---|---|
| Release packaging | New third `fono-tts` variant, ≤ 32 MiB | **No new variant.** TTS is absorbed into the existing **CPU** + **Vulkan** builds. (Fono will have at most those two; possibly a single GPU-only build in future.) |
| Inference substrate | Piper VITS + ONNX-Runtime fallback for Kokoro | **ggml-reuse** for both, sharing the runtime Fono already links via whisper.cpp + llama.cpp. ONNX rejected (+12–22 MB to a binary everyone downloads). |
| Piper license | "MIT" (stale) | **GPL-3.0** — fine for Fono (GPL-3.0-only); cleaner, not a blocker. |
| Size argument | "variant is opt-in, size matters less" | Bytes now land in the canonical binary, so **size discipline matters more** — which is exactly why ggml-reuse (smallest growth) wins over candle / ONNX. |
| shared-ggml (ADR 0022 Task 1.2) | "soft open question" | **Hard prerequisite** for cheap ggml-TTS, and a standalone ~7 MB win. Pulled to Phase 1. |
| Kokoro feasibility | "decide at Phase 2 kickoff" | **Explicit spike, Phase 3** — placed *after* Piper ships (Phase 2b) and *immediately before* the Kokoro implementation (Phase 4). |
| Wyoming TTS server | Phase 3 (last) | **Phase 2a (first code)** — decoupled from the local engine; it only needs a `TextToSpeech` impl, which already exists (cloud + Wyoming-client). Ships the HA win before any local engine. |

### Why ggml-reuse, not candle or ONNX

The "no variant" + "possible future GPU-only" constraints both push to ggml:

- **Smallest growth on the binary everyone downloads.** ggml is already
  linked; reusing it adds only per-model graph code (~1–3 MB) + static
  espeak-ng (~2–4 MB). With shared-ggml landed first, there is no third
  ggml copy. candle would add ~6–15 MB of Rust `.text`; ONNX Runtime
  +12–22 MB minimum.
- **Rides the existing Vulkan backend** (and any future GPU-only
  collapse) for free. candle has no production Vulkan backend.
- **Performance is not a differentiator at this scale** (Piper ~3–10×
  realtime, Kokoro ~1–4× realtime on CPU), so ggml's kernels buy us
  nothing we need — we pick it for *size + runtime reuse*, not speed.

Cost accepted by the project owner: **more engineering** (hand-porting
two model graphs onto ggml; forking a second sys crate for shared-ggml).
The Kokoro graph is the genuine risk and is gated behind a spike.

## Background — what already ships (re-verified 2026-05-31)

- **TTS trait + factory.** `crates/fono-tts/src/traits.rs:17-46`
  (`TextToSpeech::synthesize` → mono f32 PCM `TtsAudio`);
  `crates/fono-tts/src/factory.rs:40-54` dispatches by `TtsBackend`. A
  local engine is just another backend.
- **Wyoming codec (client side).** Synthesize/audio-* events already
  decoded by today's *client* in `crates/fono-tts/src/wyoming.rs`.
- **Wyoming server (STT-only today).**
  `crates/fono-net/src/wyoming/server.rs:1-523`. Connection handler at
  `:257-358` dispatches `Describe`/`Transcribe`; `build_info` at
  `:443-474` advertises only `asr: vec![…]` + `..Info::default()`.
  `SttProvider` closure (`:147`) gives hot-reload. A parallel
  `TtsProvider` + `Synthesize` arm is the whole of Phase 2a.
- **mDNS advertiser/browser** with an additive comma-separated `caps`
  TXT tag — `crates/fono-net/src/discovery/`. Adding `"tts"` is
  non-breaking.
- **`[server.wyoming]` config + `fono install --server`** hardened unit.
- **Generic SHA-256-pinned downloader** with Range-resume + progress UI
  — `crates/fono-download/src/lib.rs`. Reused as-is for espeak-ng-data,
  Piper voices, Kokoro weights.
- **Audio resample/playback** — `rubato`, `cpal` (`Cargo.toml:77-78`).
- **ggml runtime** — linked once via whisper.cpp + llama.cpp. Today two
  private copies coexist via `--allow-multiple-definition` (ADR 0018);
  Phase 1 collapses them to one shared build (ADR 0022 Task 1.2).

## Pinned decisions (carried from chat 2026-05-31)

| Decision | Choice |
|---|---|
| Romanian support | **Required.** Drives Piper inclusion. |
| Engine per language | **Kokoro where trained, Piper fallback** via router. |
| Inference substrate | **ggml-reuse** for both engines. No ONNX, no candle. |
| Release packaging | **No new variant.** TTS in the CPU + Vulkan builds. |
| `libespeak-ng` | **Bundled statically.** ~3 MB; no usable pure-Rust multilingual phonemizer exists. |
| espeak-ng data / Piper voices / Kokoro weights | **Downloaded at runtime**, SHA-256-pinned, via `fono-download`. Never bundled. |
| shared-ggml | **Deferred to post-Piper cleanup** (Option B, chosen 2026-05-31). Piper ships on the existing `--allow-multiple-definition` link trick at a temporary +~7 MB; shared-ggml stays the eventual ADR 0022 Task 1.2 win, no longer a blocker. |
| Kokoro-ggml feasibility | **Spike** after Piper ships, before Kokoro impl. |
| Phase order | **Task 0 → Wyoming server → Piper (on link trick) → shared-ggml cleanup → Kokoro spike → Kokoro.** (Reordered 2026-05-31, Option B.) |

## Size budget (no variant — absorbed into canonical builds)

ADR 0022's 20 MiB CPU cap is the constraint. Net effect on the CPU build:

- **−~7 MB** from shared-ggml (Phase 1, dedups the duplicate ggml copy).
- **+~1–3 MB** Piper graph code (Phase 2b).
- **+~2–4 MB** static espeak-ng (Phase 2b).
- **+~1–2 MB** Kokoro graph code (Phase 4, if the spike greenlights it).

Net is roughly flat-to-slightly-up versus today's ~18 MB. **Task 0
amends ADR 0022** to (a) record there is no `fono-tts` variant and (b)
set the post-TTS CPU cap from a real measurement after Phase 2b — target
≤ 24 MiB, only raised from 20 MiB if shared-ggml savings don't fully
offset the additions. The Vulkan build inherits the same additions under
its existing 64 MiB cap.

## Implementation Plan

### Task 0 — ADRs and budget reframe (docs only; completable immediately)

- [ ] **0.1** Amend `docs/decisions/0022-binary-size-budget.md`: add a
  2026-05-31 amendment stating local TTS is **absorbed into the CPU +
  Vulkan builds — no third variant**; TTS engines **must reuse the
  shared ggml** (depends on Task 1.2); the CPU cap is re-measured after
  Phase 2b (target ≤ 24 MiB). Update the CI size-budget matrix note.
- [ ] **0.2** Amend `docs/decisions/0004-default-models.md`: add a
  **TTS defaults** section — Piper **GPL-3.0** (`OHF-Voice/piper1-gpl`),
  Kokoro **Apache-2.0**; both GPL-3.0-compatible; neither is
  Llama/Gemma. Correct any "Apache/MIT" framing.
- [ ] **0.3** Add a banner to the v1 plan pointing here; this v2 is the
  source of truth.

### Phase 1 — shared-ggml (DEFERRED to post-Piper cleanup; Option B)

> **Status (2026-05-31): deferred, not on the Piper critical path.** A
> feasibility spike settled the cost question, and the owner chose
> **Option B** — ship Piper first on the existing
> `--allow-multiple-definition` trick (a third ggml consumer, temporary
> +~7 MB), and land shared-ggml afterward as a pure size-reclaim pass.
>
> **Spike findings (why this is not a quick flag-flip):**
> - `whisper-rs-sys-0.15.0/build.rs` has **no external-ggml knob**; it
>   unconditionally CMake-builds whisper.cpp's bundled ggml and links
>   `static=ggml{,-base,-cpu}` (`build.rs:312-316`). The v1 plan's
>   "preferred: set `GGML_USE_EXTERNAL`" path does **not exist** in
>   upstream — only the fork-and-drop-ggml fallback is viable.
> - The two ggml copies are **different revisions**: `ggml.h` differs by
>   **77 lines** (whisper 102,112 B vs llama fork 104,314 B), and the
>   llama fork carries newer backends absent from whisper's tree
>   (`ggml-backend-dl`, `ggml-backend-meta`, `ggml-ext.h`,
>   `ggml-openvino`, `ggml-virtgpu`). Sharing one binary therefore
>   requires **ABI reconciliation** (bump whisper.cpp to a ggml that
>   matches the llama fork, then keep them in lockstep), not just a
>   build-script edit — plus a published `whisper-rs-sys` fork for the
>   `[patch.crates-io]` git pin.
>
> When picked up, this reactivates ADR 0022 Task 1.2 / the ADR 0018
> rollback plan. Steps below are the eventual checklist.

- [ ] **1.1** Fork `whisper-rs-sys` (mirror the existing
  `bogdanr/llama-cpp-rs` fork pattern; add the git source to
  `deny.toml` like `Cargo.toml:191-193` does for llama). Either set the
  whisper.cpp CMake "use external ggml" knob, or drop the vendored ggml
  subtree and link the ggml `llama-cpp-sys-2` builds in its `OUT_DIR`.
- [ ] **1.2** Plumb the built-ggml path between the two sys crates via
  `links`/`DEP_*` build metadata (fork edits on both crates).
- [ ] **1.3** Reconcile the acceleration flag matrix so one ggml build
  serves both whisper.cpp and llama.cpp (CPU + each `accel-*`).
- [ ] **1.4** Delete `-Wl,--allow-multiple-definition` from
  `.cargo/config.toml:37,44`. Mark ADR 0018 **Superseded**, ADR 0022
  Task 1.2 **done**.
- [ ] **1.5** Verify: `nm $bin | grep ' [Tt] ggml_init$'` → exactly one
  entry *structurally*; `crates/fono/tests/local_backends_coexist.rs`
  passes; size-budget gate shows the ~7 MB reclaim.

> **Risk note:** this is the heaviest infra task (two forked build
> systems, ABI lockstep on every upgrade). It is a hard prerequisite for
> the ggml-TTS size math; if it stalls, Piper-ggml still links via the
> existing `--allow-multiple-definition` trick at a temporary +~7 MB cost
> (documented fallback, not the ship state).

### Phase 2a — Wyoming TTS server endpoint (decoupled; first code win)

Depends only on a `TextToSpeech` impl (cloud / Wyoming-client already
exist) — **not** on the local engine. ~150–250 LOC on existing infra.

- [x] **2a.1** Extend `fono-net-codec` Wyoming types: `Synthesize { text,
  voice, language }`, `TtsProgram { name, attribution, installed,
  description, version, voices, supports_synthesize_streaming }`,
  `TtsVoice { name, languages, … }`, a `tts: Vec<TtsProgram>` field on
  `Info`, and a `SYNTHESIZE` event const.
- [x] **2a.2** Add `TtsProvider = Arc<dyn Fn() -> Arc<dyn TextToSpeech>>`
  to `WyomingServer` parallel to `SttProvider` (`server.rs:147-160`).
  Hot-reload semantics identical.
- [x] **2a.3** `Synthesize` arm in `handle_connection`: invoke
  `tts.synthesize(text, voice, lang)`, emit `audio-start` (rate/width/
  channels) → `audio-chunk*` → `audio-stop`. Chunk per
  `TtsAudio.sample_rate` so HA pipelines first audio early.
- [x] **2a.4** `build_info` populates `tts` whenever a `TtsProvider` is
  bound; ASR + TTS coexist on one listener (Wyoming multiplexes by event
  type).
- [x] **2a.5** `[server.tts]` config block (`enabled`, `voices`,
  `default_voice`), loaded by `fono install --server` like
  `[server.wyoming]`.
- [x] **2a.6** mDNS: add `"tts"` to the `caps` tag when `[server.tts]`
  is enabled. Round-trip test.
- [x] **2a.7** Tests: synthesize round-trip in
  `crates/fono-net/tests/`; `caps` contains `tts` assertion; `build_info`
  tts-branch unit test mirroring the existing asr one
  (`server.rs:502-522`).
- [ ] **2a.8** HA verification: HA → Wyoming Protocol → discovers Fono
  as TTS; `tts.speak` returns audio. Document in `docs/providers.md`.

### Phase 2b — Piper-on-ggml local engine

> **Prerequisite findings (2026-05-31) — scope correction.** Three
> building blocks the "not so difficult" estimate assumed are **not in
> place today**, verified against `Cargo.lock` + the sys-crate sources:
> 1. **No ggml API is exposed to our own code.** There is no standalone
>    `ggml`/`ggml-sys` crate in the tree, and `whisper-rs-sys` declares
>    no `links` key, so it does not re-export ggml to downstream crates.
>    Building a Piper graph means adding a ggml binding (vendor a
>    `ggml-sys`, or write the graph in C and link it) — not just calling
>    an existing one.
> 2. **No espeak-ng binding exists** (`espeak*` absent from `Cargo.lock`).
>    The static `libespeak-ng-sys` binding in 2b.3 is net-new.
> 3. **Piper voices ship as ONNX, not GGUF.** Running them on ggml needs
>    an ONNX→GGUF weight conversion step *plus* a hand-written VITS +
>    HiFi-GAN compute graph. There is no off-the-shelf GGUF Piper.
>
> Net: 2b.2 is a genuine model port of the **same risk class as the
> Kokoro graph**, not a binding. Recommended gate before sinking engine
> code: a short **Piper-ggml micro-spike** (settle items 1–3 — pick the
> ggml-binding approach, prove ONNX→GGUF for one `ro_RO-mihai-medium`
> voice, stand up espeak-ng phonemization) mirroring the Phase 3 Kokoro
> gate. Phase 2a already ships the HA value, so this gating costs no
> user-facing capability.

> **Piper-ggml micro-spike RESULTS (2026-05-31) — outcome: GO, substrate
> decision pending owner.**
> - **Phonemization solved in pure Rust.** The `espeak-ng` crate
>   (`eugenehp/espeak-ng-rs`, GPL-3.0-or-later — compatible) is a
>   from-scratch pure-Rust espeak-ng port: `text_to_ipa("ro", text)`,
>   319 tests, bit-identical-to-C oracle, **embeddable per-language data**
>   (`bundled-data-ro`) — no system lib, no new `NEEDED`, no runtime data
>   download. **Deletes tasks 2b.3 and 2b.4.**
> - **Voice format verified.** `ro_RO-mihai-medium.onnx.json` is
>   `phoneme_type: espeak` with an **IPA-keyed** `phoneme_id_map`
>   (`num_symbols: 256`, single speaker) — a direct match for the espeak
>   crate's IPA output via the standard Piper `^`/`_`/`$` id algorithm.
> - **No ggml/GGUF Piper exists.** Every Rust Piper (`piper-rs`, its
>   ancestor `sonata`) and the upstream ecosystem run the `.onnx` via
>   ONNX Runtime (`ort`). The ggml path is a from-scratch VITS +
>   stochastic-duration + HiFi-GAN graph **plus** an ONNX→GGUF converter,
>   with no reference to crib.
> - **No downstream-exposed ggml.** Available ggml crates (`qts_ggml_sys`,
>   `ggml-sys`) vendor their own ggml → a **4th copy** unless shared-ggml
>   (deferred, Option B) lands first.
>
> **Substrate options (pick one; only the net changes):** (A) ggml port
> from scratch — smallest, highest risk, rides Vulkan Piper never needs;
> (B, recommended) **candle** pure-Rust VITS port — no new deps, fully
> pure-Rust engine with the espeak crate, CPU-only is fine, ~6–15 MB;
> (C) `ort`/`piper-rs` — works today, +12–22 MB ONNX Runtime (rejected on
> size). **Awaiting owner pick; then a short ADR records it and 2b.1+
> proceed on the chosen substrate.**
>
> **STRATEGIC PIVOT (2026-05-31, owner): Fono is committed to a FULL LOCAL
> VOICE STACK** — local TTS (Piper + Kokoro), wake-word, streaming STT,
> neural VAD, punctuation, speaker-ID. That reframes the substrate choice:
> it is no longer "an engine for Piper" but "the runtime for the whole
> voice stack." Under that lens **sherpa-onnx / `ort` (ONNX Runtime,
> Apache-2.0)** becomes the leading option — one Apache-2.0 runtime, with a
> first-party Rust API, covering Piper + Kokoro + Matcha (TTS), Zipformer
> (streaming STT, which whisper.cpp cannot do), Silero (neural VAD),
> transducer KWS wake-word (better than openWakeWord: custom phrase by
> tokens, no per-word model training), punctuation and speaker-ID. The
> ~19 MiB cost is paid **once** and amortised across 6+ roadmap features
> instead of being charged to one engine. ggml stays the LLM runtime;
> whisper STT can stay on ggml now and optionally migrate to ONNX later.
>
> **Static-ONNX spike RESULTS (2026-05-31) — outcome: technically CLEAN,
> size cost is the only open decision.** Measured with a scratch crate
> depending on `ort = "2.0.0-rc.12"` (wraps onnxruntime 1.26), built
> `release` + LTO + `opt-level=s` + `strip`:
> - **onnxruntime links STATICALLY** — `ort`'s default `download-binaries`
>   pulls a prebuilt **static** `libonnxruntime.a` (87 MB unstripped on
>   disk) and embeds it. **No `libonnxruntime.so` in `NEEDED`** — verified
>   by `readelf -d`. Meets the "no external deps except runtime downloads"
>   rule. (Models still download at runtime; only engine *code* is in the
>   binary.)
> - **`NEEDED` reduces to exactly Fono's four-entry allowlist** — `{libc,
>   libm, libgcc_s, ld-linux}` — once libstdc++ is linked statically.
>   `ort-sys` (`build/static_link/mod.rs:20-32`) emits a *dynamic*
>   `cargo:rustc-link-lib=stdc++`; Fono already forces static libstdc++ for
>   ggml's C++ (the `llama-cpp-2/static-stdcxx` feature). Proven in the
>   spike by resolving `-lstdc++` against a search dir containing only
>   `libstdc++.a` → `libstdc++.so.6` dropped, binary still runs
>   (`ort init committed: true`).
> - **Measured size delta: ~19.24 MiB** of `.text`+data over an empty
>   baseline for a minimal `ort::init()` binary (20,473,016 B with static
>   c++ vs 299,688 B baseline). onnxruntime registers operators via static
>   tables, so `--gc-sections` cannot prune most kernels — this ~19 MiB is
>   a realistic estimate, not a gc-deflated under-count. **Consequence: the
>   canonical binary roughly doubles, ~19 MiB → ~37–40 MiB**, and (per the
>   no-variant decision) every user downloads it.
> - **Build-infra note:** `download-binaries` fetches from pyke's CDN at
>   build time. For reproducible/offline CI, pin a vendored static
>   `libonnxruntime.a` via `ORT_LIB_LOCATION` rather than relying on the
>   network fetch. Not a ship blocker; a release-engineering task.
> - **Precedent:** `ort`'s README lists Fono-shaped projects already on
>   this stack — `sbv2-api` (Style-BERT-VITS2 TTS), Murmure & SilentKeys
>   (Parakeet STT + Silero VAD + LLM dictation).
>
> **The one decision left to the owner:** accept the **~+19 MiB / canonical
> binary roughly doubling to ~37–40 MiB** in exchange for the whole voice
> stack on one Apache-2.0 runtime. If accepted → rewrite this plan around
> ONNX (substrate option D), amend ADR 0022 (raise the `cpu` cap to
> ~40 MiB; ONNX replaces the "ggml-reuse, no ONNX" line) and ADR 0004
> (per-model licensing for Piper GPL / Kokoro Apache), then start
> Piper-on-`ort` as the stack's first consumer. If the size doubling is
> unacceptable → fall back to candle (option B) for Piper alone and accept
> a separate, smaller engine that does not amortise across the stack.
>
> **Follow-up research (2026-05-31) — shrinking the ~19 MiB, HA comparison,
> Vulkan:**
> - **The ~19 MiB is the FULL prebuilt (all ops + all EPs). It is highly
>   reducible.** ONNX Runtime supports a custom **minimal build** tuned to
>   our exact, fixed model set: `--minimal_build --include_ops_by_config
>   <ops.config> --enable_reduced_operator_type_support --disable_ml_ops
>   --disable_exceptions --disable_rtti --config MinSizeRel`, with models
>   converted to **ORT format**. This is the same path ONNX Runtime Mobile
>   uses (mobile minimal builds land ~5–7 MiB). Realistic target for
>   Fono's fixed op set (Piper VITS + Kokoro + Silero VAD + Zipformer +
>   KWS): **~7–11 MiB**, roughly **halving** the measured cost. Mechanism:
>   build our own static `libonnxruntime.a` in CI, pin it via
>   `ORT_LIB_LOCATION` (ort's `download-binaries` is then off). This is a
>   release-engineering task, not a code task, and is the primary lever if
>   the size is accepted.
> - **`ort` exposes the EP/size knobs we need:** `xnnpack` (statically
>   linkable CPU accelerator — the right CPU speed-up), plus `cuda`,
>   `coreml`, etc. No `minimal` cargo feature — reduction is done in the
>   onnxruntime build we pin.
> - **Home Assistant's Piper is NOT smaller or special.** `wyoming-piper`
>   is a thin *Python* Wyoming wrapper around the `piper` C++ binary, which
>   links the **full `libonnxruntime`** (~15–20 MB, dynamic) and ships as a
>   **Docker add-on** (hundreds of MB container). HA pays the *same*
>   onnxruntime cost we measured — they just ship it dynamically inside a
>   container instead of statically in one binary. There is no lightweight
>   Piper engine to copy; our number matches their reality. (The Wyoming
>   *server* glue itself is tiny — which we already have, Phase 2a.)
> - **Vulkan + ONNX: effectively no, and unnecessary.** ONNX Runtime has
>   **no Vulkan EP**. Cross-vendor GPU = DirectML (Windows-only) or WebGPU
>   via Dawn; `ort-sys` (`build/static_link/mod.rs:64-67`) ships Dawn as a
>   **dynamic** lib ("Dawn cannot be linked statically yet"), which would
>   **break the four-entry `NEEDED` allowlist**. And it is moot: Piper,
>   Kokoro, Silero, KWS and small/streaming STT are all **CPU-realtime**,
>   so GPU buys nothing. Fono's existing **ggml-Vulkan** stays the path for
>   the only GPU-hungry workloads (whisper-large, the LLM); the ONNX voice
>   stack stays CPU (XNNPACK for any speed-up). The two runtimes split
>   cleanly along the GPU boundary.

- [ ] **2b.1** `tts-local` feature on `crates/fono-tts` (off by default
  in source; **on** in the shipped CPU + Vulkan builds). Pulls the
  ggml-backed Piper engine + a static `libespeak-ng-sys` binding.
- [ ] **2b.2** Port the Piper VITS + HiFi-GAN graph onto ggml (the
  in-house engineering piece). Weights loaded from GGUF-converted Piper
  voices. *Scope honestly: this is a real model port, not a binding.*
- [ ] **2b.3** Bundle `libespeak-ng` statically with the same `-Os
  -ffunction-sections -fdata-sections` + `--gc-sections` treatment as
  whisper.cpp/llama.cpp. Data dir not compiled in; point runtime at
  `~/.cache/fono/espeak-ng-data/` via `espeak_ng_InitializePath`.
- [ ] **2b.4** Lazy espeak-ng-data downloader via `fono-download`
  (per-language `.dict` + shared `phontab`/`phonindex`, SHA-256-pinned).
- [ ] **2b.5** Piper voice catalogue + downloader (`OHF-Voice` /
  `rhasspy/piper-voices` on HuggingFace; GGUF-converted; curated
  medium-or-better per locale). Cache `~/.cache/fono/models/piper/`.
- [ ] **2b.6** `PiperTts: TextToSpeech`. Per-sentence synthesis using
  the existing `SentenceSplitter` (`crates/fono-tts/src/lib.rs:35`) for
  time-to-first-audio. Empty text → empty PCM (trait contract).
- [ ] **2b.7** Factory wire-up + `[tts.local]` config; default voice =
  first catalogue match for the user's language; honour `voice =`.
- [ ] **2b.8** Wizard + doctor + tray exposure (local engine status,
  voice cache size, espeak-ng-data version).
- [ ] **2b.9** Tests: downloader cache layout, voice resolver, factory
  round-trip; smoke test synthesising a Romanian + an English phrase
  (output PCM non-silent, expected length band).
- [ ] **2b.10** Size-budget re-measure → finalise the ADR 0022 CPU cap.
  Docs / changelog / roadmap for the shipping release.

### Phase 3 — Kokoro-on-ggml feasibility spike (gate before Phase 4)

Placed here per the project owner: only after Piper ships, immediately
before committing to the Kokoro implementation.

- [ ] **3.1** Survey: does a usable ggml / GGUF Kokoro (StyleTTS2-family)
  implementation exist to adapt, or is the compute graph a from-scratch
  port? Catalogue any candidate ports, their license, their maturity.
- [ ] **3.2** Prototype the riskiest sub-graph (the decoder / vocoder
  path) on ggml with one English voice; measure RTF + quality on a
  4-core CPU.
- [ ] **3.3** Decide: **go** (Phase 4 as scoped), **defer** (Piper-only
  ships; Kokoro tracked), or **fall back** (Kokoro via cloud router
  only). Record the outcome as a short ADR.

### Phase 4 — Kokoro-on-ggml + language router (gated on Phase 3)

- [ ] **4.1** Kokoro ggml engine under `tts-local`; GGUF weights +
  styles via `fono-download`.
- [ ] **4.2** G2P for the Kokoro locales (misaki-equivalent or its Rust
  port); Piper keeps espeak-ng for the long tail.
- [ ] **4.3** `KokoroVoiceRouter::pick_voice(BCP-47) -> Option<&str>`;
  `None` → Piper. Engine dispatcher in `factory.rs` builds a
  `LocalTtsRouter` wrapping both.
- [ ] **4.4** Wizard: unified "Local" picker, "auto by language" default.
- [ ] **4.5** Tests: router per language (ro→Piper, fr/en→Kokoro,
  pl→Piper); two-language round-trip smoke test.
- [ ] **4.6** Promote `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`
  Phase 1 to closed/superseded. Size re-measure; docs / changelog /
  roadmap.

## Verification criteria

- **Phase 1:** one structural `ggml_init`; `local_backends_coexist`
  passes; size-budget shows the reclaim; `--allow-multiple-definition`
  gone.
- **Phase 2a:** vanilla HA on the LAN discovers Fono under "Wyoming
  Protocol" within 30 s of `[server.tts].enabled`; `tts.speak` returns
  audio; `caps=tts` round-trips; ASR + TTS serve from one daemon/port.
- **Phase 2b:** CPU build within the (re-measured) cap; `NEEDED` still
  the four-entry allowlist (no new dynamic deps; espeak-ng static); cold
  install downloads espeak-ng-data + one Piper voice and synthesises the
  first sentence < 30 s on a 4-core CPU; Romanian reply reads back in
  `ro_RO-mihai-medium`; per-sentence first-audio p50 < 300 ms.
- **Phase 3:** a written go/defer/fallback decision backed by a measured
  RTF + quality sample.
- **Phase 4:** `["en","ro","fr"]` user hears Kokoro for en/fr, Piper for
  ro, switching automatically per reply.

## Out of scope

- Voice cloning / custom fine-tunes.
- WebSocket streaming TTS (Wyoming audio-chunk stream suffices for HA).
- Wyoming wake-word service (separate ROADMAP item).
- A dedicated GPU path for TTS (CPU is realtime; Vulkan reuse is free if
  it happens, not a goal).
- macOS / Windows local-TTS builds (Linux first; follow-up after soak).

## Risks and mitigations

1. **shared-ggml fork maintenance.** Two forked sys-crate build systems
   on ABI lockstep. Mitigation: pin both to the same ggml family; CI
   smoke test on every bump; documented `--allow-multiple-definition`
   fallback.
2. **Kokoro ggml port is a research project.** The load-bearing unknown.
   Mitigation: the Phase 3 spike gates Phase 4; Piper-only is a complete,
   shippable product without Kokoro.
3. **Piper VITS ggml port effort underestimated.** It is a model port,
   not a binding. Mitigation: scoped explicitly in 2b.2; Phase 2a (HA
   server over cloud/Wyoming backends) ships value even if 2b slips.
4. **CPU budget pressure.** Absorbing TTS may push the CPU build over
   20 MiB. Mitigation: shared-ggml reclaim first; re-measure and amend
   the cap with a real number in Task 0 / 2b.10.
5. **Piper GPL-3.0.** Fine for Fono (GPL-3.0-only); update ADR 0004
   cross-references (Task 0.2).
6. **espeak-ng data SHA churn / HA Wyoming drift.** Pin SHAs per upstream
   release; Phase 2a.8 verifies against current stable HA.

## Cross-links

- `plans/2026-05-25-local-tts-piper-kokoro-and-wyoming-server-v1.md` —
  superseded by this plan.
- `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md` — its Phase 1
  (local backend + router) is subsumed by Phase 4 here; cloud-parity
  stays open there.
- `docs/decisions/0022-binary-size-budget.md` — amended by Task 0.1.
- `docs/decisions/0018-ggml-link-trick.md` — superseded by Phase 1.
- `docs/decisions/0004-default-models.md` — amended by Task 0.2.
- `plans/closed/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`
  — the foundation Phase 2a builds on.
