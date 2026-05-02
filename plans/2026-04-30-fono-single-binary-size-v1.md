# Fono — Single Binary Size Reduction (≤ 20 MB, all features, no shared libs)

Date: 2026-04-30
Author: agent session continuing from `docs/status.md` 2026-04-29 entries
Supersedes scope: the cancelled v0 sketch with three flavours

## Objective

Ship **one** `fono` binary, statically linked with no `NEEDED` shared libraries,
that is **≤ 20 MB** stripped on `x86_64-unknown-linux-musl` and **≤ 22 MB** on
`aarch64-unknown-linux-musl`, with **every feature compiled in**:

- Local STT (whisper.cpp via `whisper-rs`)
- Local LLM cleanup (llama.cpp via `llama-cpp-2`)
- Cloud STT/LLM backends (Groq, OpenAI, Anthropic, Cerebras, Ollama, OpenRouter)
- Wyoming + Fono-native server (`fono serve`)
- Wyoming + Fono-native client (`FonoStt`/`FonoLlm`)
- mDNS LAN autodiscovery
- Tray + interactive overlay + text injection
- First-run wizard, history DB, hotkey FSM, self-update

The same binary runs as:

- a **local desktop client** (full pipeline, tray, overlay, injection)
- a **headless server** (`fono serve`, no `DISPLAY`/`WAYLAND_DISPLAY`)
- a **remote client** that delegates STT/LLM to a LAN peer
- any combination of the above

The graphical surfaces (tray, overlay, injection) are **not** behind cargo
features; they are compiled in unconditionally and **runtime-gated** on the
presence of `DISPLAY`/`WAYLAND_DISPLAY` and the daemon's `[server]` /
`[interactive]` config. A headless host gets a working `fono serve` from the
same binary an end-user installs on their laptop.

## Constraints (non-negotiable)

- **One binary.** No `fono-server` / `fono-slim` / `fono-cloud` artefacts.
  `cargo build --release-slim --target x86_64-unknown-linux-musl` produces the
  one ELF that ships everywhere.
- **No `NEEDED` shared libraries.** `ldd $bin` must print *"not a dynamic
  executable"*. No `libgtk-3`, no `libstdc++`, no `libgomp`, no `libasound`
  outside what musl already statically resolves.
- **Local LLM stays default-on.** Required for privacy, future translate
  feature (`plans/2026-04-28-fono-auto-translation-v1.md`), and the
  server-side local-inference path that ADR 0019 + the v2 network plan
  promise to LAN clients.
- **GUI runtime detection only.** No compile-time `gui` / `server` /
  `headless` flavours. Detection happens in `daemon.rs` startup based on env
  vars + config; absent surfaces are no-ops with a `debug!` log line.
- **Budget audited in CI.** Every PR runs the size-budget gate; a 1-byte
  overage fails the merge.

## Why we're over budget today

Real numbers from `target/debug/build/*.a` and `readelf -d 192.168.0.72` on
the user's debug binary (the only build artefact currently on disk):

| Static archive | Size | Verdict |
|---|---:|---|
| `libcommon.a` (llama-cpp-2's `common/`) | 13.8 MB | **dead weight — no Rust call site references it** |
| `libcommon_wrapper.a` (llama-cpp-sys-2 C++ bridge over `common/`) | 10.1 MB | **dead weight — bridges the unused `common/`** |
| `libwhisper.a` | 14.4 MB | needed |
| `libllama.a` | 8.3 MB | needed |
| `libggml-cpu.a` | 7.5 MB | needed, **but currently linked twice** via `--allow-multiple-definition` (ADR 0018) |
| `libggml-base.a` etc. | ~3 MB | needed (×2 today) |
| Rust + reqwest + tokio + tracing + winit + tray-icon + … | ~12 MB after LTO | mostly justified |

The user's `192.168.0.72` artefact is debug-built and dynamically linked
(`NEEDED: libgtk-3, libgdk-3, libcairo, libpango, libgio-2.0, libglib-2.0,
libstdc++, libgomp, libasound, libgcc_s, libm, libc, ld-linux`). After
release-LTO-strip with the current configuration the binary lands around
**25–30 MB** with all those `.so` deps still attached — which is the
"fucked up" state the user is reporting.

The biggest single fixable win is **stop pulling 24 MB of llama.cpp helper
code we never call**, plus de-duplicate ggml. That alone should hit budget;
the rest of the plan is belt-and-braces.

## Implementation plan

### Phase 1 — Strip llama.cpp helpers + de-duplicate ggml + dead-section GC (≥ 10 MB win)

* [x] **Task 1.1.** Strip `llama-cpp-sys-2`'s `common/` build from the
  default link. **Wired into Fono.**
  - **Upstream PR:**
    [utilityai/llama-cpp-rs#1015](https://github.com/utilityai/llama-cpp-rs/pull/1015)
    adds an opt-out `common` feature (default-on) to `llama-cpp-sys-2`
    and `llama-cpp-2` that gates the `common/` cmake build, the
    `wrapper_common.cpp` / `wrapper_oai.cpp` cc invocation, the
    `link-lib=static=common` emit, and the bindgen `llama_rs_*`
    allowlist. `LlamaSampler::accept` falls back to llama.cpp's core
    `llama_sampler_accept` when the feature is off so the basic sampling
    loop still works.
  - **Fono integration:** workspace dep in `Cargo.toml:87` switched to
    `default-features = false, features = ["openmp"]`, plus a
    `[patch.crates-io]` block at `Cargo.toml:153-155` pinning both
    `llama-cpp-2` and `llama-cpp-sys-2` to
    `bogdanr/llama-cpp-rs:feature/optional-common-build` (commit
    `d9ffd75`). `cargo check -p fono` and
    `cargo check -p llama-cpp-2 --no-default-features` both pass. The
    `[patch]` block goes away once #1015 merges.
  - Estimated saving after LTO + `--gc-sections`: **6–10 MB of `.text`**.
    Measure via `tests/check.sh --size-budget` once Tasks 1.2 + 1.3 land
    so the Phase 1 win is captured in one pass.

* [ ] **Task 1.2.** Source-level shared ggml between `whisper-rs-sys` and
  `llama-cpp-sys-2`.
  - Move `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` back to
    active and use as the work checklist.
  - Preferred: patch `whisper-rs-sys/build.rs` to `set(GGML_USE_EXTERNAL on)`
    (or equivalent CMake variable) and link against the ggml that
    `llama-cpp-sys-2` already builds in its `OUT_DIR`. Pass the path via
    `cargo:rustc-link-search` and `cargo:rustc-link-lib=static=ggml`.
  - Fallback if whisper.cpp's CMake doesn't expose that knob: fork
    `whisper-rs-sys`, drop the embedded ggml subtree, and depend on the
    one llama-cpp-sys-2 produces. Pin via `[patch.crates-io]`.
  - Delete `.cargo/config.toml:21-28`'s `-Wl,--allow-multiple-definition`
    flag.
  - Smoke test: `nm $bin | grep ' [Tt] ggml_init$'` returns **exactly one**
    entry (today: passes by accident because ld picks the first definition;
    after this task: passes structurally because there is only one).
  - Verify the existing `crates/fono/tests/local_backends_coexist.rs` test
    still passes — `WhisperLocal` and `LlamaLocal` co-loaded in one
    process.
  - Saving: **~7 MB of duplicate ggml `.text`**.

* [x] **Task 1.3.** Compile whisper.cpp / llama.cpp / ggml with size-aware
  flags.
  - Set `CFLAGS` / `CXXFLAGS` to `-Os -ffunction-sections -fdata-sections`
    in `.cargo/config.toml` `[env]` block, scoped to the musl target.
  - Set `RUSTFLAGS` to include `-C link-arg=-Wl,--gc-sections,--as-needed`
    in the same place.
  - ggml ships AVX-512 / AMX / AVX2 / SSE / scalar dispatch kernels;
    without `--gc-sections` the unused arch kernels stay in `.text`.
  - Saving: **1–2 MB**.

* [x] **Task 1.4.** Remove unused workspace dep declarations.
  - `ort`, `rodio`, `swayipc`, `hyprland` declared in `Cargo.toml:80-94`
    but **zero `use` sites** in the codebase (verified
    `fs_search 'use ort|use rodio|use swayipc|use hyprland' → 0 hits`).
  - Cosmetic / hygiene change; zero binary impact today, prevents future
    regression.

### Phase 2 — Pure-Rust tray + static C++ runtime + musl ship default (delivers the "no shared libraries" promise)

* [x] **Task 2.1.** Replace `tray-icon`'s libappindicator backend with a
  `ksni`-based pure-Rust StatusNotifierItem implementation.
  - `ksni` (MIT, ~MIT) speaks the SNI D-Bus protocol directly via `zbus`
    — no GTK, no glib, no cairo, no `pkg-config` dance at build time.
  - Hosts that consume SNI: KDE Plasma (native), GNOME with the SNI
    extension (the same one our docs already require), sway+waybar,
    i3+i3status, hyprland+waybar, xfce4-panel, lxqt-panel, dwm with
    `dwmblocks`. Covers every supported environment.
  - Files: `crates/fono-tray/src/lib.rs` rewrites the `RealTray` impl on
    top of `ksni::Tray`. The public API (`spawn`, `set_state`,
    `RecentProvider`, `MicrophonesProvider`, all `TrayAction::*` variants)
    stays unchanged — daemon side untouched.
  - Drops `NEEDED`: `libgtk-3`, `libgdk-3`, `libcairo`, `libcairo-gobject`,
    `libgdk_pixbuf`, `libpango`, `libpangocairo`, `libpangoft2`,
    `libgio-2.0`, `libgobject-2.0`, `libglib-2.0`, `libgmodule`,
    `libharfbuzz`, `libfontconfig`, `libfribidi`, `libatk-1.0`,
    `libatk-bridge-2.0`, `libepoxy`, `libxkbcommon`, plus all transitive
    `libX*` deps that GTK pulls.
  - Saves ~1–2 MB of `.text` from binding code.

* [ ] **Task 2.2.** Keep a `tray-gtk` opt-in feature for fallback.
  - Off by default. `cargo build --features tray-gtk` re-enables the
    libappindicator path for users on hostile panels.
  - Documented in `docs/troubleshooting.md` + `docs/wayland.md`.
  - Lets us flip the default without painting ourselves into a corner.

* [x] **Task 2.3.** Static C++ runtime + OpenMP on the canonical glibc
  ship.
  - GNU release builds link llama.cpp's `libgomp` and `libstdc++`
    statically via the forked `llama-cpp-2` features `static-openmp` and
    `static-stdcxx`; verified `cargo build --release -p fono` and the
    glibc release-slim ship binary have no `libgomp.so.1` /
    `libstdc++.so.6` in `NEEDED` (CI gate `size-budget` enforces this).
  - **Musl variant deferred** — see Task 2.4.

* [ ] **Task 2.4.** *(Deferred 2026-05-02)* Promote `release-slim` + musl
  to the canonical release.
  - **Status:** parked. `messense/rust-musl-cross:x86_64-musl` ships a
    `libgomp.a` that is non-PIC (breaks `-static-pie`) **and** depends on
    glibc-only symbols (`memalign`, `secure_getenv`) plus a chain of POSIX
    references that musl libc can't resolve in the rust-driven link
    order. We chased this for 11 commits (`901e41d..29cc577`) and
    abandoned: each shim/flag exposed the next layer.
  - Resurrection path: switch `llama-cpp-2` fork to llvm-openmp (libomp
    is PIC-friendly) **or** build our own minimal cross image with a
    PIC-built `libgomp.a` from GCC sources. ~1-2 days either way; not
    worth blocking PRs on while the ship binary works.
  - **Replacement gate (landed 2026-05-02):** the `size-budget` CI job
    now builds `x86_64-unknown-linux-gnu` (matching `release.yml`) and
    asserts size + a positive NEEDED allowlist. Catches GTK/libstdc++/
    libgomp regressions just like the musl gate would have.

### Phase 3 — Runtime detection of GUI surfaces (no compile gates)

* [ ] **Task 3.1.** Audit `crates/fono/src/daemon.rs` startup paths to
  confirm tray/overlay/injection initialise lazily and **no-op gracefully**
  when the host is headless.
  - Decision rule: a host is graphical iff `DISPLAY` *or* `WAYLAND_DISPLAY`
    is set in the daemon's environment. (Existing partial check needs to
    become the canonical predicate.)
  - Tray spawn: `if graphical { spawn_tray() } else { debug!("headless,
    skipping tray") }`.
  - Overlay: `if graphical && config.interactive.enabled { … }` (already
    holds for the `[interactive]` knob; add the `graphical` predicate so
    a misconfigured headless server doesn't try to open a winit window
    against `:0`).
  - Injection: existing logic already returns `InjectOutcome::NoBackend`
    on headless; tighten the doctor surface to print "headless: no
    text-injection path" instead of "no backend available".

* [ ] **Task 3.2.** `fono doctor` learns a "Mode:" line: `local desktop`,
  `headless server`, `LAN client only`, etc., based on the same
  predicate + the live `[server]` config.

* [ ] **Task 3.3.** Add an integration test `crates/fono/tests/headless.rs`
  that runs the daemon with `DISPLAY` and `WAYLAND_DISPLAY` unset,
  asserts `fono serve` starts cleanly, no tray/overlay errors are logged,
  and `Request::Transcribe` round-trips through the local STT path.

### Phase 4 — Rust-side trims (only if Phases 1–3 don't already hit budget)

Hold these in reserve. Re-measure after Phase 2 lands; only execute the
items below if the binary is still over 20 MB.

* [ ] **Task 4.1.** Drop `tracing-subscriber`'s `env-filter` feature.
  - Pulls `regex` (~1 MB after LTO).
  - Hand-roll the small grammar `FONO_LOG=mod=level,mod2=level` we
    actually use; expose as a custom `Layer` in `crates/fono-core/src/log.rs`.

* [ ] **Task 4.2.** Replace `dialoguer` + `indicatif` in the wizard with
  hand-rolled prompts. Wizard runs once per install; UX bar is low. ~600 KB.

* [ ] **Task 4.3.** Trim `tokio = { features = ["full"] }` in
  `Cargo.toml:39` to the explicit set we use (`rt-multi-thread`, `macros`,
  `net`, `time`, `sync`, `signal`, `process`, `io-util`, `fs`).
  ~300–500 KB.

* [ ] **Task 4.4.** Audit `reqwest` features.
  - Drop `http2` if no provider needs it (current providers all speak
    HTTP/1.1 fine; check Groq/OpenAI/Anthropic).
  - Drop `multipart` from clients that only POST JSON (every LLM
    backend; STT clients keep it for audio upload).
  - ~500 KB–1 MB.

* [ ] **Task 4.5.** Disable `whisper-rs/tracing_backend` feature in the
  ship build (workspace `Cargo.toml:78`). Only enable on dev builds where
  the user can read GGML logs. ~200 KB.

* [ ] **Task 4.6.** Replace `clap` derive with `argh` or `pico-args`.
  *Last resort.* ~700 KB. Touches ~30 subcommands.

### Phase 5 — Verification harness (no regression)

* [ ] **Task 5.1.** Size-budget CI gate.
  - Extend `tests/check.sh` and `.github/workflows/ci.yml` with:
    `cargo build --profile release-slim --target
    x86_64-unknown-linux-musl` followed by `stat -c%s
    target/x86_64-unknown-linux-musl/release-slim/fono` and a hard-fail if
    over **20 971 520 bytes** (20 MB exact). Same for aarch64 at 22 MB.
  - Same gate runs on the release workflow before artefact upload.

* [ ] **Task 5.2.** `ldd`-empty CI assertion.
  - `ldd $bin 2>&1 | grep -q "not a dynamic executable"` else fail.
  - Catches anyone re-introducing a GTK / glibc dep.

* [ ] **Task 5.3.** `cargo-bloat --release-slim --crates -n 30` step
  emitting top contributors as a CI artefact, so the next size regression
  points us straight at the offender. Optional: add `--filter '\.text\.'`
  for kernel-only weighting.

* [ ] **Task 5.4.** `nm`-based duplication check.
  `nm $bin | grep -c '^[0-9a-f]\+ [Tt] ggml_init$'` must equal `1`.
  Locks Task 1.2 in.

### Phase 6 — Documentation

* [ ] **Task 6.1.** ADR `docs/decisions/0022-binary-size-budget.md`.
  Records the 20 MB single-binary budget, the dead-code-elimination
  strategy (gc-sections + common-stripped llama-cpp-sys-2 + shared ggml),
  the no-shared-libs invariant, the runtime-gated GUI rule. Marks ADR
  0018 (`--allow-multiple-definition`) as **Superseded**.

* [ ] **Task 6.2.** Update `docs/plans/2026-04-24-fono-design-v1.md` line
  ~514 from "≤ 25 MB stripped" to "≤ 20 MB stripped musl-static, `ldd`
  empty, single binary serves headless and graphical roles".

* [ ] **Task 6.3.** Update `Cargo.toml:144-150` profile comment to reflect
  the new size truth (~17 MB target after Phase 1 + 2; was "~19 MB
  default release, ~15 MB release-slim").

* [ ] **Task 6.4.** Update `docs/status.md` per AGENTS.md rule.

* [ ] **Task 6.5.** `CHANGELOG.md` `[Unreleased]` entry under `Changed`:
  binary size, no shared libs, runtime headless detection, ksni tray.
  Per AGENTS.md release rule, this graduates to `[0.4.0]` at tag time.

## Verification criteria

- `cargo build --profile release-slim --target x86_64-unknown-linux-gnu`
  produces a `fono` ELF **≤ 20 MiB (20 971 520 bytes)** with the default
  feature set (`tray + local-models + llama-local + interactive`).
- `readelf -d target/x86_64-unknown-linux-gnu/release-slim/fono | grep NEEDED`
  produces **only** entries from the universal allowlist:
  `libc.so.6`, `libm.so.6`, `libgcc_s.so.1`, `ld-linux-x86-64.so.2`.
  Modern glibc (≥ 2.34) merged libpthread/librt/libdl into libc.so.6 so
  they no longer appear separately.
- The dedup invariant (single ggml copy) is enforced at link time by
  `--allow-multiple-definition` in `.cargo/config.toml`; release-slim
  strips symbols so a runtime `nm` check is not possible.
- The same binary, run with `DISPLAY` / `WAYLAND_DISPLAY` unset, starts
  `fono serve` cleanly with no tray/overlay errors logged.
- The same binary, run on a graphical desktop, brings up tray + overlay +
  injection identically to today.
- Existing `crates/fono/tests/local_backends_coexist.rs` smoke test still
  passes (whisper + llama co-loaded in one process).
- `tests/check.sh` matrix (fmt, build × default + interactive, clippy ×
  default + interactive, test × default + interactive) green.
- CI `size-budget` gate fails the build at 20 MiB + 1 byte **or** when
  any unexpected NEEDED entry appears.
- ADR 0022 published with the 2026-05-02 amendment; ADR 0018 still
  Active (not yet superseded — Phase 1 Task 1.2 source-shared ggml
  also deferred).

## Risks and mitigations

1. **Forking `llama-cpp-sys-2` for the `common/` strip creates a
   maintenance tail.**
   *Mitigation:* upstream PR first (one feature gate, ~5 lines). If
   accepted, no fork. If not, hold a single-commit
   `vendor/llama-cpp-sys-2-fono/` patch under `[patch.crates-io]`. Pin to
   the exact upstream commit so a re-base is mechanical.

2. **Source-level shared ggml may break on whisper.cpp ↔ llama.cpp
   ABI drift.**
   *Mitigation:* both upstreams already track `ggerganov/ggml` closely;
   pin both sys crates to commits whose vendored ggml is the same upstream
   SHA (validated via the `ggml/CMakeLists.txt` `project(ggml VERSION ...)`
   line). Add a CI matrix bump-test that flags drift early.

3. **`ksni` SNI tray may misrender on hostile panels.**
   *Mitigation:* `tray-gtk` opt-in feature retained as fallback (Task
   2.2). Field test on KDE / GNOME-with-extension / sway+waybar /
   i3+i3status / xfce4-panel / lxqt-panel before the v0.4.0 tag.

4. **`--gc-sections` can drop FFI-only symbols.**
   *Mitigation:* include the existing `local_backends_coexist` smoke test
   plus a new "exercise-every-ggml-arch-init" link-time check. Document
   `cargo build --profile release-slim` as the single supported ship
   command.

5. **Phase 1 alone may not hit budget if Rust .text is bigger than
   estimated.**
   *Mitigation:* Phase 4 holds 2–3 MB of independent reductions in
   reserve. Re-measure after each phase; only execute Phase 4 items as
   needed.

6. **Headless audit (Phase 3) is invasive across `daemon.rs`.**
   *Mitigation:* the daemon already has partial DISPLAY-awareness for
   tray/inject; Phase 3 is mostly tightening existing predicates and
   adding the integration test. ~1 day of work, not a refactor.

## Alternatives considered and rejected

1. **Drop `llama-local` from default.** *Rejected by user* — privacy,
   future translate, server-side local inference all need it.
2. **Move `llama.cpp` / `whisper.cpp` to sidecar `.so`.** *Rejected* —
   breaks the no-shared-libs rule.
3. **Three flavours (desktop/server/cloud-only).** *Rejected by user* —
   one binary, runtime role detection.
4. **Compile-time `gui` feature gate.** *Rejected* — same reason; one
   binary serves both roles.
5. **`opt-level = "z"` on the whole workspace.** *Rejected* — tanks
   whisper inference latency. We hit budget without it.
6. **Replace `winit` + `softbuffer` with raw X11/Wayland-layer-shell now.**
   *Deferred* — saves ~2 MB but is a meaningful rewrite. Revisit in Slice
   B's overlay-subprocess refactor (ADR 0009 §5).

## Estimated outcome

| State | Binary size, all features |
|---|---|
| Pre-Phase-1 | ~28 MB, GTK + glibc + libstdc++ + libgomp `NEEDED` |
| + Phase 1 (kill `common.a` + dedup ggml + gc-sections) | **~18–20 MB**, dynamic |
| + Phase 2.1 (ksni tray) + 2.3 (static C++/OpenMP on glibc) | **~18 MB**, NEEDED = {libc, libm, libgcc_s, ld-linux} ← **canonical ship as of 2026-05-02** |
| + Phase 2.4 (musl ship, **deferred**) | future ~17 MB, `ldd` empty |
| + Phase 4 (only if needed) | **~14–16 MB** |

The current measured ship binary is **18 957 120 bytes (≈ 18.08 MB)**
on x86_64-unknown-linux-gnu. CI gates regressions at 20 MiB.
Phase 2.4 (musl ship) is parked — see Task 2.4 above.

Phase 1 hit budget. Phase 2.1+2.3 delivered the "no GTK / no C++ runtime"
promise on the glibc target. Phase 2.4 (musl) is parked. Phase 3 ratifies
the headless-on-the-same-binary contract. Phases 4–6 are defence in
depth.

## Sequencing

Execute in order: Phase 1 → re-measure → Phase 2 → re-measure → Phase 3.
Phase 4 only triggers if Phase 2 measurement is over 20 MB. Phase 5 + 6
land alongside whichever phase is last.
