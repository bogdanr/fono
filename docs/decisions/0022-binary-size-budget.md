# ADR 0022 — Binary size budget: 20 MiB glibc-dynamic ship with NEEDED allowlist

## Status

Accepted 2026-04-30. **Amended 2026-05-02:** the canonical ship target
is `x86_64-unknown-linux-gnu` (glibc-dynamic) with a positive `NEEDED`
allowlist, not `x86_64-unknown-linux-musl` (no NEEDED at all). The
20 MiB budget stands; the no-shared-libraries invariant is replaced
by an allowlist of universal glibc-stack libs. ADR 0018
(`--allow-multiple-definition`) **stays Active** — Phase 1 Task 1.2
(source-shared ggml) is still pending, so the link kludge still
carries the dedup invariant. ADR 0022 will supersede ADR 0018 once
Task 1.2 lands.

> **AMENDED 2026-06-24 — the source-shared-ggml reclaim is ≈ 0 MiB, not
> ~7 MiB; Task 1.2 is deferred indefinitely as a size optimisation.**
> A spike re-measured the duplicate on the canonical `release-slim`
> `x86_64-unknown-linux-gnu` `cpu` artefact (26.60 MiB). A non-stripped
> relink shows `ggml_init` defined **once**, **zero** duplicated ggml
> globals, single-copy ggml `.text` ≈ 1.03 MiB. The `~7 MiB` figure
> repeated below is an *archive-size* inheritance that does **not**
> survive the link: `-ffunction-sections -fdata-sections` +
> `-Wl,--gc-sections` already collect the loser ggml copy's sections, so
> `--allow-multiple-definition` ships a single copy. **Every "~7 MiB
> reclaim", "offset for ONNX", and "ADR 0022 supersedes ADR 0018 once
> Task 1.2 lands" statement below is superseded by this measurement.**
> ADR 0018 is now the **steady state**, not an interim kludge; Task 1.2's
> only residual benefit is build time (ggml compiled twice), which the
> size budget does not count. If revisited, the front-runner is
> upstreaming `system-ggml` (llama-cpp-sys-2 already ships it; only a
> `whisper-rs-sys` Codeberg fork/PR would remain) — triggered by
> correctness or build-time needs, not size. See
> `plans/2026-06-23-shared-ggml-size-reclaim-spike-v1.md` and
> `docs/binary-size.md` §4.

> **AMENDED 2026-06-24 (part 2) — `cpu` budget raised to 28 MiB, hard cap
> lowered to 30 MiB.** The enforced `cpu` size-gate row moves from 27 MiB
> to **28 MiB (29 360 128 B)** in `.github/workflows/ci.yml` (both the
> x86_64 and aarch64 rows): wake-word work consumed the last of the 27 MiB
> headroom and CI measured the artefact just over the line on its runner
> toolchain (local builds were ~26.6 MiB). The hard `cpu` cap is tightened
> from ≤ 32 MiB to **≤ 30 MiB**, leaving ~2 MiB of deliberate ceiling above
> the enforced row. These figures supersede the 26/27 MiB budget and
> ≤ 32 MiB cap stated elsewhere in this ADR. `gpu` is unchanged.

> **AMENDED 2026-07-01 — `release-slim` adopts `opt-level = "s"`; `cpu`
> budget tightened to 25 MiB, hard cap to 28 MiB.** The `release-slim`
> profile previously inherited `opt-level = 3` (speed) from `release`;
> it now sets **`opt-level = "s"`** (size) in `Cargo.toml`. Measured
> 2026-07-01 on `x86_64-unknown-linux-gnu` (default features), this drops
> the shipped artefact from **26.60 MiB to 21.64 MiB (−4.96 MiB)** with
> the four-entry `NEEDED` allowlist intact and **no feature loss** — the
> saving is duplicated Rust glue codegen, and the C/C++ inference core
> (whisper/llama/ggml/onnxruntime, compiled with its own `-Os`/cmake
> flags) is untouched, so inference throughput is unchanged. The measured
> ladder: `3` = 26.60, `2` = 25.99, `"s"` = 21.64, `"z"` = 20.39 MiB;
> `"z"` was rejected as it disables Rust loop vectorisation. To bank the
> win, the enforced `cpu` size-gate row moves from **28 MiB to 25 MiB
> (26 214 400 B)** in `.github/workflows/ci.yml` (both x86_64 and aarch64
> rows), and the hard `cpu` cap from **≤ 30 MiB to ≤ 28 MiB**, preserving
> a ~3 MiB gap above the enforced row for CI toolchain variance and the
> aarch64 artefact. These figures supersede the 28 MiB budget / ≤ 30 MiB
> cap of the 2026-06-24 amendment. `gpu` is unchanged. See
> `docs/binary-size.md` §2.

> **AMENDED 2026-07-04 — macOS artefact joins the budget matrix.** The
> macOS port (plans/2026-07-03-macos-port-v1.md) ships a **single
> Metal-accelerated variant** for `aarch64-apple-darwin` (no cpu/gpu
> split — measured Phase 3: `accel-metal` costs +0.65 MiB while
> transcribing ~4.3× faster; ggml falls back to its CPU backend at
> runtime). A new `size-budget-macos` job in `.github/workflows/ci.yml`
> builds the exact `release-slim --features accel-metal` ship artefact
> and asserts:
>
> - **Enforced budget ≤ 18 MiB (18 874 368 B); hard cap ≤ 20 MiB.**
>   Measured 2026-07-04 on the Mac Studio bench: **16 143 328 B
>   (15.40 MiB)**, ~2.6 MiB headroom under the enforced row.
> - **`LC_LOAD_DYLIB` allowlist (the Mach-O analogue of the Linux
>   `NEEDED` gate): exactly 17 entries**, all system frameworks
>   (`/System/Library/Frameworks/…` — Accelerate, AppKit,
>   ApplicationServices, AudioUnit, Carbon, CoreAudio, CoreData,
>   CoreFoundation, CoreGraphics, CoreServices, Foundation, Metal,
>   MetalKit) or `/usr/lib` system libraries (`libSystem.B`,
>   `libc++.1`, `libiconv.2`, `libobjc.A`). The static-link posture
>   carries over: onnxruntime, whisper/llama/ggml, and the C++ runtime
>   are embedded; a new import is a leaked dependency and fails CI.
>
> The darwin numbers live in lockstep with the `size-budget-macos` job;
> change them together, budget bumps only with sign-off recorded here.
> Linux budgets are unaffected.

> **AMENDED 2026-07-13 — Windows joins the budget matrix as a single
> Vulkan build with CPU fallback.** The Windows port ships **one**
> `x86_64-pc-windows-msvc` artefact (`fono-vX.Y.Z-x86_64.exe`, feature
> set `windows-defaults`) that is Vulkan-accelerated *and* runs
> everywhere — GPU when a usable `vulkan-1.dll` driver is present, CPU
> fallback when it isn't. Unlike Linux (which keeps the compact `cpu` /
> Vulkan `gpu` two-variant split so the ~42 MB SPIR-V shader payload is
> opt-in), Windows accepts the shader payload for **every** user in
> exchange for a single no-choice download — a deliberate
> simplicity-over-size trade for a target the maintainer rarely tests.
> See `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`,
> `docs/build-windows.md` ("Vulkan single build"), and `docs/status.md`.
>
> - **Enforced budget ≤ 60 MiB (62 914 560 B); hard cap ≤ 64 MiB** —
>   the same ceiling family as the Linux `gpu` variant (both carry the
>   ggml-vulkan shaders). Bump only with sign-off recorded here.
> - **PE import-table allowlist (the Windows analogue of the Linux ELF
>   `NEEDED` gate) must NOT contain `vulkan-1.dll`.** The in-tree loader
>   shim (`crates/fono-core/src/vk_loader_shim.rs`) resolves ggml's
>   three bare Vulkan symbols itself and `LoadLibraryA`s the loader
>   lazily, so `vulkan-1.dll` stays out of the import table and the
>   `.exe` launches even with no GPU driver. Verified 2026-07-13 on the
>   Windows 10 bench: `dumpbin /DEPENDENTS` shows no `vulkan-1.dll`;
>   transcription runs on GPU when the loader is present and falls back
>   to CPU (exit 0, no crash) when it is absent. The remaining PE
>   imports are the standard Win32 system DLLs + the MSVC/UCRT runtime.
> - **Not yet CI-gated.** The dumpbin import-table + size assertion (the
>   `windows`-job analogue of `size-budget` / `size-budget-macos`) is
>   deferred to Windows port Phase 14, along with promoting the
>   non-blocking `windows` CI job to a required check. When it lands it
>   asserts both the budget (see the 2026-07-14 amendment below) and the
>   `vulkan-1.dll`-absent invariant. Linux and macOS budgets are
>   unaffected.

> **AMENDED 2026-07-14 — Windows budget raised to ≤ 75 MiB.** Enabling
> local (offline) text-to-speech and the wake-word engine on Windows —
> which links the statically-embedded ONNX Runtime, matching Linux and
> macOS — added ~3 MiB and pushed the single `fono-vX.Y.Z-x86_64.exe`
> from ~69 MiB to ~72 MiB, over the ≤ 60 MiB figure set on 2026-07-13
> (that figure predated local TTS landing on Windows). With sign-off,
> the Windows budget is raised to:
>
> - **Enforced budget ≤ 75 MiB (78 643 200 B); hard cap ≤ 80 MiB
>   (83 886 080 B).** This supersedes the ≤ 60 MiB / ≤ 64 MiB figures in
>   the 2026-07-13 amendment above. It leaves the ~72 MiB measured
>   artefact ~3 MiB of headroom for CI toolchain variance. Windows is a
>   single no-choice download the maintainer rarely tests, so the ceiling
>   is a loose sanity bound, not a tight ship-size target; the strict
>   budgets stay on Linux `cpu` (≤ 25 MiB) and macOS. Bump only with
>   sign-off recorded here. When the Phase 14 `windows`-job size gate
>   lands it asserts this ≤ 75 MiB figure. Linux and macOS budgets are
>   unaffected.


The static-musl ship (Phase 2.4) is **deferred** — see "Rejected:
static-musl with libgomp" in Trade-offs.

**Amended 2026-05-02 (part 2):** GPU acceleration ships as a **second
release variant** rather than a default-on feature. Local measurement
showed enabling `accel-vulkan` adds ~42 MB (150+ precompiled SPIR-V
shaders + ggml-vulkan C++) — a 3× size blow-up that's incompatible
with the 20 MiB budget. The two-variant approach (compact CPU default
+ optional `fono-gpu` build) honours the budget for the canonical
download while still delivering GPU to users who want it. The CI
size-budget gate is now a matrix:

- `cpu`: ≤ 20 MiB, NEEDED ⊆ {`libc.so.6`, `libm.so.6`,
  `libgcc_s.so.1`, `ld-linux-x86-64.so.2`}.
- `gpu`: ≤ 64 MiB, NEEDED ⊆ above + `libvulkan.so.1`.

See `plans/2026-05-02-fono-cpu-gpu-variants-v1.md` for the variant
plumbing, runtime detection, and upcoming upgrade UX.

**Amended 2026-05-31 (local TTS — no third variant):** local
text-to-speech does **not** ship as a third release artefact. Fono will
have **at most** the existing two builds — `cpu` and `gpu` (Vulkan) —
and may collapse to a single GPU-only build in future; it will not
fragment further.

> **SUPERSEDED 2026-05-31 (part 2) by ADR 0032.** The paragraph below
> assumed local TTS would be hand-ported onto the shared ggml runtime
> ("no ONNX, no candle"). That premise was reversed once Fono committed
> to a **full local voice stack** (TTS + wake-word + streaming STT +
> neural VAD + speaker-ID). Per ADR 0032, the voice stack runs on
> **ONNX Runtime, statically linked via `ort`** — one Apache-2.0 runtime
> for all those model classes. The ggml-reuse requirement and the
> "shared-ggml is a hard prerequisite for TTS" claim no longer hold:
> shared-ggml is now a *size-offset* task (Phase after Piper), not a
> blocker. The binding constraints that survive — **no third variant**,
> **four-entry `NEEDED` allowlist**, **engine code never bundled as a
> `.so`** — are unchanged and are satisfied by static ONNX (verified
> 2026-05-31; see ADR 0032). Read the paragraph below as historical.

The (now-superseded) ggml-reuse consequences were:

- Local TTS engines (Piper, later Kokoro) are **absorbed into the `cpu`
  and `gpu` builds**, behind a `tts-local` cargo feature that is off in
  source-default builds but **on** in the shipped artefacts.
- The engines **must reuse the shared ggml runtime** (no second/third
  ggml copy, no ONNX Runtime, no candle). This makes **Phase 1 Task 1.2
  (source-shared ggml) a hard prerequisite** for local TTS, not just a
  size optimisation: shared-ggml first reclaims ~7 MB, which offsets the
  Piper graph code (~1–3 MB) + static `libespeak-ng` (~2–4 MB).
- The `cpu` cap is **re-measured after the Piper engine lands** (Phase
  2b of
  `plans/2026-05-31-local-tts-ggml-piper-kokoro-and-wyoming-server-v2.md`).
  Target **≤ 24 MiB**; the 20 MiB line is raised only if the shared-ggml
  reclaim does not fully absorb the additions, and the new number is
  recorded here with the measurement. The `gpu` build inherits the same
  additions under its existing 64 MiB cap.
- `NEEDED` is unchanged: `libespeak-ng` is linked **statically**, so the
  four-entry allowlist (`cpu`) / five-entry (`gpu`, + `libvulkan.so.1`)
  still holds. Any new dynamic dep fails the gate.

**Amended 2026-05-31 (part 3 — ONNX voice stack, per ADR 0032):** the
local voice stack runs on **statically-linked ONNX Runtime** (`ort`),
not ggml. Consequences for this ADR:

- The full prebuilt onnxruntime adds **~19 MiB** (measured). Fono ships a
  **custom minimal build** (`--minimal_build --include_ops_by_config
  --enable_reduced_operator_type_support --disable_ml_ops
  --disable_exceptions --disable_rtti --config MinSizeRel`, ORT-format
  models), tuned to exactly the operators our model set uses. **Measured
  2026-05-31: the minimal build adds only ~2.1 MiB** to a release binary
  (`opt-level=s` + LTO + strip + `--gc-sections`) for the 10-operator
  Piper VITS op set — far better than the ~7–11 MiB estimate, because
  `--gc-sections` prunes the bulk of the 50 MiB archive that the fixed op
  set never references. The static `libonnxruntime.a` is built in CI and
  pinned via `ORT_LIB_LOCATION` (no CDN fetch, no `libonnxruntime.so`).
- **`NEEDED` stays four-entry, but ONNX needed a dedicated libstdc++
  fix.** `ort-sys` emits its own `-lstdc++` link directive separately
  from `llama-cpp-sys-2`; the `llama-cpp-2/static-stdcxx` mechanism does
  **not** cover it (link ordering — llama's `libstdc++.a` is already
  scanned before onnxruntime's C++ symbols resolve), so a naive
  `tts-local` build **leaks a dynamic `libstdc++.so.6`** (measured
  2026-06-01: 5-entry `NEEDED`, 25.33 MiB). The fix:
  `ORT_CXX_STDLIB="static:-bundle=stdc++"` in `.cargo/config.toml`
  (defers the static archive to the **final `fono` link**, where the
  search path is present) plus a feature-gated `crates/fono-tts/build.rs`
  that emits the `libstdc++.a` search path via
  `gcc --print-file-name=libstdc++.a`. With this, a plain
  `cargo build --features tts-local` (only `ORT_LIB_LOCATION` set, no
  manual `RUSTFLAGS`) presents exactly `{libc, libm, libgcc_s,
  ld-linux}`. Note the static link is also **~0.9 MiB smaller** than the
  dynamic leak, because `--gc-sections` prunes the unreferenced bulk of
  `libstdc++.a`.
- **New `cpu` cap: ≤ 32 MiB.** Measured `release-slim` numbers
  (2026-06-01): default (no `tts-local`) **22.52 MiB**, four-entry;
  `tts-local` with static libstdc++ **24.45 MiB** (+1.9 MiB),
  four-entry. The per-op growth of future voice models (Kokoro, Silero
  VAD, KWS, streaming STT) is the real consumer of the remaining
  headroom. The source-shared ggml dedup (Phase 3) still reclaims ~7 MiB
  as an independent offset. Record measured numbers here as they land.
  `gpu` stays ≤ 64 MiB.
- The voice stack is **CPU-only** (XNNPACK EP); ONNX has no Vulkan EP and
  does not use the `gpu` variant's Vulkan. ggml-Vulkan still serves
  whisper-large + the LLM. See ADR 0032 "CPU / Vulkan split".
- Source-shared ggml (Task 1.2) is **no longer a prerequisite** for the
  voice stack — it is reclassified as a size-offset task scheduled after
  Piper ships.

**Amended 2026-06-02 (tts-local is now source-default):** the
"off in source-default builds but on in the shipped artefacts" split
(part 1 above, line ~57) is **retired**. With both original blockers
cleared — static-libstdc++ leak fixed (`ORT_CXX_STDLIB="static:-bundle=stdc++"`)
and a hosted prebuilt `libonnxruntime.a` (pinned by SHA per triple in
`scripts/fetch-onnxruntime.sh`, served from the `bogdanr/fono-voice`
mirror, tag `onnxruntime-1.24.2`) — `tts-local` joins the default
feature set in `crates/fono/Cargo.toml`. Consequences for this gate:

- **Every compiling job now links `ort`** and therefore needs
  `ORT_LIB_LOCATION`. The fetcher runs unconditionally in the `test`
  and `size-budget` jobs (`.github/workflows/ci.yml`) and in the
  `cloud-assistant` + `build` jobs (`.github/workflows/release.yml`).
  `cloud-equivalence`/`fono-bench` is unaffected (no `ort` in its graph).
- **The dedicated `cpu-tts-local` size-budget row is removed** — the
  default `cpu` row now exercises the static onnxruntime link directly.
- **`cpu` budget raised to 26 MiB (27 262 976 B).** Measured
  `release-slim` default (now incl. `tts-local`) **2026-06-02:
  25 768 120 B (24.57 MiB)**, four-entry `NEEDED`
  (`ld-linux`, `libc`, `libgcc_s`, `libm`) — no `libonnxruntime.so`,
  no `libstdc++.so.6`. ~1.4 MiB headroom under the 26 MiB row budget;
  well under the ≤ 32 MiB hard `cpu` cap. `gpu` stays ≤ 64 MiB.

The full size-and-capability engineering is documented in
`docs/binary-size.md`.

The rejected "third `fono-tts` variant" approach (a 2026-05-25 draft) is
not adopted; see the superseded v1 TTS plan for that history.

## Context

The v1 design plan (`docs/plans/2026-04-24-fono-design-v1.md:514-516`)
promised a *single static-musl ELF ≤ 25 MB stripped, `ldd` reporting "not
a dynamic executable"*. Reality on `main` at 2026-04-29 diverged:

- The release artefact lands around **25–30 MB** stripped, depending on
  the profile.
- It links **GTK 3 + glib + cairo + libstdc++ + libgomp + libasound +
  glibc** dynamically (`readelf -d`); `ldd` is far from empty.
- `--allow-multiple-definition` (ADR 0018) keeps both copies of `ggml`
  in the link, wasting ~7 MB of `.text`.
- `llama-cpp-sys-2` unconditionally builds and links `libcommon.a`
  (13.8 MB) and `libllama_cpp_sys_2_common_wrapper.a` (10.1 MB). Fono
  doesn't reference any symbol from either archive, but the linker
  pulls them in.

User feedback: this is unacceptable. Fono is supposed to be
*"self-contained and light"*, target ≤ 20 MB with all features, ~15 MB
at the v0.4 milestone, and there must be **no shared libraries** in the
final ELF. Furthermore, the binary must **not** fragment into
desktop / server / cloud-only variants — one binary services every
role, with graphical surfaces (tray, overlay, injection)
runtime-detected from `DISPLAY` / `WAYLAND_DISPLAY` rather than gated
behind cargo features.

## Decision

Adopt a hard **20 MiB (20 971 520 bytes)** budget for the
`x86_64-unknown-linux-gnu` `release-slim` artefact with **all default
features enabled** (`tray + local-models + llama-local + interactive`).
The canonical ship build is:

```sh
cargo build -p fono --profile release-slim \
    --target x86_64-unknown-linux-gnu
```

The same binary must:

- run `fono` locally on a graphical desktop (full pipeline + tray +
  overlay + text injection);
- run `fono serve` on a headless server (tray and overlay refuse to
  spawn at runtime when `DISPLAY` and `WAYLAND_DISPLAY` are both
  unset);
- run as a Wyoming / Fono-native client to a remote peer;
- present a **`NEEDED` set that is exactly the universal glibc + libgcc_s
  ABI** present on every desktop Linux ≥ ~2018 — and nothing else:
  - `libc.so.6`
  - `libm.so.6`
  - `libgcc_s.so.1`
  - `ld-linux-x86-64.so.2`
  Modern glibc (≥ 2.34) merges `libpthread/librt/libdl` into `libc.so.6`
  so they no longer appear separately. Anything outside this allowlist
  (libgtk, libstdc++, libgomp, libayatana, libxdo, libasound,
  libxkbcommon, libwayland-*) fails the gate.
- contain exactly **one** copy of every `ggml_*` symbol. The dedup
  invariant is enforced at link time by `--allow-multiple-definition`
  in `.cargo/config.toml` (ADR 0018); release-slim sets
  `strip = "symbols"` so a runtime `nm` check is not possible. Breaking
  the invariant produces a *multiple-definition* link error, not a
  silent pass.

**Glibc symbol-version surface.** Both the `size-budget` CI gate and
`release.yml`'s build matrix are pinned to **`ubuntu-22.04`** (glibc
2.35) so the binary's `GLIBC_2.X` symbol versions stay compatible with
Ubuntu 22.04+, Debian 12+, Fedora 36+, and any glibc ≥ 2.35 host.
`ubuntu-latest` (24.04, glibc 2.39) would silently raise the floor and
exclude ~3 years of supported distros. The two workflows must stay in
lockstep — if you bump one, bump both. RHEL 9 (glibc 2.34) is just
shy of our floor and not supported; targeting it would require an
even older runner image (currently `ubuntu-20.04`, scheduled for
removal) or a manylinux-style build container.

The reductions live in
`plans/2026-04-30-fono-single-binary-size-v1.md`. In summary:

1. **Strip llama.cpp's `common/`** from the `llama-cpp-sys-2` link
   (Phase 1 Task 1.1). Fono uses none of it; saves ~6–10 MB after
   LTO + `--gc-sections`.
2. **Source-level shared ggml** between `whisper-rs-sys` and
   `llama-cpp-sys-2` (Phase 1 Task 1.2). Retires the
   `--allow-multiple-definition` link kludge from ADR 0018; saves
   ~7 MB.
3. **`-Os -ffunction-sections -fdata-sections` + `-Wl,--gc-sections`**
   on the C++ build of whisper.cpp / llama.cpp / ggml (Phase 1 Task
   1.3). Drops unused arch kernels and helper functions; saves
   1–2 MB.
4. **`ksni` pure-Rust StatusNotifierItem tray** (Phase 2 Task 2.1)
   replacing the libappindicator/GTK backend. Drops every GTK / glib /
   cairo / pango / fontconfig / X11 transitive `NEEDED` entry from
   the binary.
5. **`-static-libstdc++ -static-libgcc -l:libgomp.a`** on the musl
   target (Phase 2 Task 2.3). Drops the last `libstdc++.so.6` and
   `libgomp.so.1` `NEEDED` entries.
6. **Runtime gating of GUI surfaces** in `crates/fono/src/daemon.rs`
   (Phase 3) so the same binary runs headless cleanly. No
   compile-time `gui` / `server` features — one binary, one matrix.
7. **CI size-budget gate** in `tests/check.sh --size-budget` and the
   release workflow (Phase 5). 1 byte over budget fails the build.

The local LLM backend (`llama-local`) **stays in the default feature
set** because privacy, the future translate feature
(`plans/2026-04-28-fono-auto-translation-v1.md`), and the LAN-server
local-inference path that the v2 network plan promises all require it.

## Verification

- `cargo build -p fono --profile release-slim
  --target x86_64-unknown-linux-gnu` produces a `fono` ELF
  ≤ 20 971 520 bytes. Measured on 2026-05-02: **18 957 120 bytes
  (≈ 18.08 MB)**, leaving ~2 MB of headroom.
- `readelf -d target/.../release-slim/fono | grep NEEDED` produces
  exactly `libc.so.6 libm.so.6 libgcc_s.so.1 ld-linux-x86-64.so.2`
  (any order). Anything else fails CI.
- The same binary, started with `DISPLAY` and `WAYLAND_DISPLAY` unset,
  brings up `fono serve` cleanly with tray and overlay refusing to
  spawn (`debug!` log lines, no errors).
- The same binary on a graphical desktop brings up tray + overlay +
  injection identically to today.
- `.github/workflows/ci.yml` `size-budget` job passes in CI on every
  PR. Failure modes: size > budget, NEEDED set diverges from allowlist.
- Smoke test `crates/fono/tests/local_backends_coexist.rs` still
  passes — `WhisperLocal` and `LlamaLocal` co-load in the same
  process.

## Trade-offs

- **`llama-cpp-sys-2` `common`-strip requires a fork or upstream PR.**
  The patch is ~10 lines; we pin via `[patch.crates-io]` until upstream
  releases the gate. Maintenance tail measured in low single-digit
  hours per upstream rebase.
- **Source-level shared ggml** binds the two sys crates to compatible
  upstream ggml SHAs. We pin both crates to commits whose vendored
  ggml comes from the same `ggerganov/ggml` family; CI guards the
  smoke test on every dependency bump.
- **`ksni` SNI tray** requires StatusNotifierItem hosting on the
  user's panel. KDE and KDE-derived panels host it natively; GNOME
  needs the SNI extension (the same one our docs already require
  today); sway+waybar / hyprland+waybar / i3+i3status / xfce4-panel /
  lxqt-panel all support it. Hostile hosts fall back to the opt-in
  `tray-gtk` feature.
- **Static-libstdc++ + static-libgomp** on the canonical glibc target
  inflates the binary by ~1–2 MB compared to a fully-dynamic build, but
  drops `libstdc++.so.6` and `libgomp.so.1` from `NEEDED` — they appear
  on most desktop Linuxes but not all (e.g. minimal containers), and
  the version skew between distros makes them risky shared deps.
- **Rejected: static-musl with libgomp.** Pursued for ~11 commits
  (`901e41d..29cc577`) before being deferred 2026-05-02. The
  `messense/rust-musl-cross:x86_64-musl` image's `libgomp.a` is
  non-PIC (breaks `-static-pie`) and references glibc-only symbols
  (`memalign`, `secure_getenv`) plus a chain of POSIX symbols whose
  resolution depends on link-order details rust's driver controls.
  Each shim/flag exposed the next layer. The binary works fine
  glibc-dynamic with the four-entry NEEDED allowlist; chasing static-
  musl was buying compatibility with Alpine/Void-musl users who are
  not the target audience for a desktop voice-dictation tool. Recapture
  if/when llama-cpp-2 swaps to llvm-openmp (libomp is PIC-friendly) or
  a PIC-libgomp source build is pinned.

## Rollback path

If a future linker bug or upstream ABI break invalidates source-level
shared ggml, fall back to ADR 0018's `--allow-multiple-definition`
linker trick. The plan to do so is preserved in
`plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` — when this
ADR lands, that plan moves back to active to drive the share, and the
ADR 0018 trick remains a documented contingency.

If `ksni` proves to misrender on a critical user's panel, the
`tray-gtk` opt-in feature (`crates/fono-tray/Cargo.toml`) re-enables
the libappindicator path. The user rebuilds with
`--features tray-gtk` and accepts the +24 GTK/glib/cairo/etc. `NEEDED`
entries; the size-budget gate still passes because GTK is a runtime
dep, not bytes-in-the-binary.

## Surviving artefacts

- `.cargo/config.toml` (size flags, static C++ runtime, dup-ggml
  trick — the last retiring with Task 1.2)
- `Cargo.toml` (`[profile.release-slim]`)
- `tests/check.sh --size-budget`
- `plans/2026-04-30-fono-single-binary-size-v1.md` (work checklist)
- `plans/2026-04-30-llama-cpp-sys-2-strip-common.patch.md` (the
  upstream / fork patch)
- `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` (rollback
  path; reactivated by Task 1.2)
- `docs/decisions/0018-ggml-link-trick.md` — marked **Superseded** by
  this ADR once Task 1.2 lands.
