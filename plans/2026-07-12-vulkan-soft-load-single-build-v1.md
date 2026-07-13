# Soft-load Vulkan — no hard link, CPU fallback, single Windows build

## Status: In progress — Phase 1 (Linux soft-load) implemented and verified

Date: 2026-07-12
Author: agent session (design discussion with the maintainer)

## Objective

Make the Vulkan-accelerated inference backend (ggml-vulkan, reached via
`whisper-rs/vulkan` and `llama-cpp-2/vulkan`) **load the Vulkan loader
softly** — i.e. never as a hard link / `NEEDED` (ELF) or import-table
(PE) entry — and **fall back to CPU** at runtime when the loader or a
usable device is absent. A Vulkan-enabled binary must therefore *launch
and run everywhere*, using the GPU when present and CPU otherwise.

This unlocks two decisions the maintainer has taken:

1. **Windows ships a single Vulkan-with-fallback `.exe`.** This
   supersedes the "CPU-only v1, GPU variant deferred" decision in
   `plans/2026-05-26-windows-port-v1.md` (Task 3.4, Phase 5.1,
   Phase 14.3). Rationale: **simplicity is the priority on Windows** —
   the maintainer rarely runs the Windows binary, so one artefact with
   automatic GPU-or-CPU behaviour beats a two-variant matrix + runtime
   probe + self-update variant-switching to maintain and test. The cost
   (a larger `.exe` carried by everyone) is accepted on Windows.

2. **The Linux GPU variant stops hard-linking `libvulkan.so.1`.** This
   is the long-deferred item flagged in
   `plans/closed/2026-05-02-fono-cpu-gpu-variants-v1.md:323-325`
   ("Patching ggml-vulkan … to dlopen libvulkan … would let the GPU
   variant drop libvulkan from NEEDED"). After this change the Linux
   GPU build launches on hosts without the Vulkan loader and degrades to
   CPU instead of dying at the dynamic linker.

## Non-goals / explicitly out of scope

- **Collapsing Linux to a single build.** Linux keeps the two-variant
  model (compact CPU default `fono`, optional `fono-gpu`). The 42 MB
  SPIR-V shader payload still violates the Linux "compact, runs on every
  distro" promise and the 25 MiB `cpu` size budget
  (`docs/decisions/0022-binary-size-budget.md`), so the CPU default
  stays the primary Linux download. The only Linux change here is that
  the *GPU* variant becomes launch-safe (soft-load + CPU fallback). The
  runtime Vulkan probe and self-update auto-switch between `fono` and
  `fono-gpu` (`crates/fono-update/src/lib.rs:184-212`) are unchanged.
- **CUDA / ROCm / DirectML backends.** Vulkan remains the single GPU
  answer (per the closed variants plan). No new acceleration backend.
- **macOS.** Ships one Metal variant; Metal is a system framework, not a
  dlopen concern. Untouched.
- **Changing the Windows release asset name.** It stays
  `fono-vX.Y.Z-x86_64.exe` (single artefact). No `-gpu` suffix on
  Windows, so `asset_name_for` / `desired_asset_prefix`
  (`crates/fono-update/src/lib.rs:164-212`) need only comment/rationale
  updates, not logic changes.

## Background — why it hard-links today

There are two separate pieces of Vulkan wiring; only one is the problem:

- **The probe** already loads softly. `ash` is pulled with the `loaded`
  feature (`Cargo.toml:202-208`), which `dlopen`s the loader via
  `libloading`; it is deliberately *not* the `linked` feature. This is
  how the CPU variant detects GPUs without adding `libvulkan.so.1` to
  its NEEDED set. Nothing to change here.
- **The inference backend** is what hard-links. `whisper-rs/vulkan` →
  `whisper-rs-sys` → vendored ggml, and likewise the `bogdanr/llama-cpp-rs`
  fork (`Cargo.toml:304-313`), build ggml with `GGML_VULKAN=ON`, whose
  CMake does `find_package(Vulkan REQUIRED)` and links the
  `Vulkan::Vulkan` import target. That import library is what puts
  `libvulkan.so.1` in NEEDED (ELF) / `vulkan-1` in the PE import table.
  ggml-vulkan itself dispatches through Vulkan-Hpp's dynamic dispatcher,
  so the *link-time* dependency is likely heavier than the code actually
  requires — which is exactly what the Phase 0 spike must confirm.

## Design

### The two soft-load mechanisms

- **Linux (and the mechanism-agnostic ideal):** stop linking the
  `Vulkan::Vulkan` import library; link only the headers and let ggml's
  Vulkan-Hpp dynamic dispatcher `dlopen("libvulkan.so.1")` at init.
  Candidate approaches, to be chosen in Phase 0:
  1. Link `Vulkan::Headers` only (drop `Vulkan::Vulkan`) if ggml's
     dispatcher already loads the loader dynamically — smallest patch.
  2. Build ggml with volk (`GGML_VULKAN_*` volk path, if the pinned
     ggml exposes it) for an explicit dynamic loader.
  3. `VK_NO_PROTOTYPES` + manual `dlopen` shim if neither of the above
     lands cleanly.
  Whatever is chosen becomes a patch on our whisper-rs-sys build and the
  `bogdanr/llama-cpp-rs` fork (pin the fork commit in `Cargo.toml` per
  the existing convention).
- **Windows:** the cheap, native path is MSVC delay-loading —
  `/DELAYLOAD:vulkan-1.dll` plus `delayimp.lib` and a delay-load failure
  handler — so `vulkan-1.dll` is not touched until the first Vulkan
  call. This can be driven from the `[target.x86_64-pc-windows-msvc]`
  rustflags block in `.cargo/config.toml` (same place `/FORCE:MULTIPLE`
  lives). If the header-only link (Linux approach 1) also resolves the
  PE import, delay-load may be belt-and-suspenders; Phase 0 decides.

### The CPU fallback is mostly already there

ggml-vulkan enumerates device count at init and, finding zero devices,
leaves work on the CPU backend. So once the *loader load* is soft (no
crash when the DLL/`.so` is missing, no crash when it loads but reports
no devices), "fall back to CPU" is the backend's existing behaviour. The
work is making the loader load lazy/optional and guarding any
Vulkan-symbol reference so nothing is called before the device probe.

### Windows feature set

Windows drops the `windows-defaults`-minus-Vulkan story and instead
builds the Vulkan set. The `ort`-pulling features (`tts-local`,
`wakeword-onnx`) remain excluded until a merged static `onnxruntime.lib`
is provisioned (unchanged from today); this plan only adds
`accel-vulkan` to the Windows build. Net Windows v1 feature set becomes:
default minus `tts-local`/`wakeword-onnx` **plus** `accel-vulkan`.

### Sizes and budgets

| Build | Today | After |
|---|---|---|
| Linux `cpu` (`fono`) | ~21 MiB | unchanged (no Vulkan) |
| Linux `gpu` (`fono-gpu`) | ~60 MB, NEEDED +`libvulkan.so.1` | ~60 MB, NEEDED back to the 4-entry allowlist |
| Windows `.exe` | ~15.7 MiB (CPU-only) | ~55–60 MB (Vulkan + shaders) |

- Linux `cpu` gate and 4-entry NEEDED allowlist are untouched (the whole
  point of keeping two Linux variants).
- Linux `gpu` NEEDED allowlist **shrinks** from 5 back to 4 — this is a
  win and should be asserted by the gpu size-budget job.
- The Windows size budget rises from the planned ~30 MiB
  (`plans/2026-05-26-windows-port-v1.md:716-720`) to ~60 MiB. This is a
  deliberate, signed-off trade of size for simplicity on Windows and
  must be recorded in `docs/decisions/0022-binary-size-budget.md` (or a
  new ADR) — the Windows budget is independent of the Linux `cpu` hard
  cap.

## Implementation plan

### Phase 0 — Spike the linkage mechanism (no shipped change)

- [x] Task 0.1. **Determine why `libvulkan.so.1` lands in NEEDED today.**
      *(Done 2026-07-12.)* Root cause pinpointed in the vendored ggml
      shared by both `whisper-rs-sys` 0.15.0 and the
      `bogdanr/llama-cpp-rs` fork:
      - `ggml-vulkan.cpp:9` sets `VULKAN_HPP_DISPATCH_LOADER_DYNAMIC 1` —
        **ggml dispatches the vast majority of Vulkan calls through a
        runtime dispatcher**, initialised at `ggml-vulkan.cpp:5401`
        (`ggml_vk_default_dispatcher_instance.init(vkGetInstanceProcAddr)`).
      - Artifact-level confirmation (built `fono-bench
        --no-default-features --features accel-vulkan`, debug): the
        binary's `NEEDED` includes `libvulkan.so.1`, driven by
        **exactly 3** bare (non-dispatched) Vulkan symbols — `nm -D`
        reports `vkGetInstanceProcAddr` (bootstrap, `:5401`),
        `vkGetPhysicalDeviceFeatures2` (direct calls at
        `ggml-vulkan.cpp:4862,5348,15171`), and `vkCmdCopyBuffer`
        (direct calls at `:6313,6384,6535`). Any soft-load fix must
        cover all three, not just the bootstrap.
      - `ggml/src/ggml-vulkan/CMakeLists.txt:89`
        (`target_link_libraries(ggml-vulkan PRIVATE Vulkan::Vulkan)`)
        links the full import lib to satisfy that one symbol; the
        `bogdanr/llama-cpp-rs` `build.rs:883` also emits
        `cargo:rustc-link-lib=vulkan` (`vulkan-1` on Windows, line 864).
        `whisper-rs-sys` relies on the CMake link-interface propagation
        (its vulkan block only adds bindgen headers).
- [x] Task 0.2. **Pick the mechanism.** *(Done 2026-07-12.)* A minimal
      C experiment (`/tmp/vkspike`, transient) proved the fix and the
      root cause together:
      - Program A references a bare Vulkan symbol and links `-lvulkan` →
        `readelf -d` shows `NEEDED libvulkan.so.1`. Dropping `-lvulkan`
        leaves the bare symbol as the sole unresolved reference —
        confirming the loader edge is symbol-driven.
      - Program B fetches the entry point via `dlopen("libvulkan.so.1")`
        + `dlsym`, links **without** `-lvulkan` (only `-ldl`) → `NEEDED`
        is just `libc.so.6`, and it runs, resolving the loader at
        runtime with a clean loader-absent → CPU-fallback branch.
      **Chosen mechanism (single, cross-platform):** resolve the 3 bare
      symbols (Task 0.1) through a dynamically-loaded loader instead of
      link-time, and stop linking the loader import lib.
      `vk::detail::DynamicLoader` (available; host header
      `VK_HEADER_VERSION 341 ≥ 301`) `dlopen`s the loader and uses
      `LoadLibrary("vulkan-1.dll")` on Windows — so **the same fix
      addresses both OSes and likely makes the MSVC `/DELAYLOAD` trick
      unnecessary.** Two implementation shapes to choose between in
      Phase 1:
      1. **Source patch** to `ggml-vulkan.cpp`: route the bootstrap and
         the 6 direct call sites (`vkGetPhysicalDeviceFeatures2` ×3,
         `vkCmdCopyBuffer` ×3) through `VULKAN_HPP_DEFAULT_DISPATCHER`,
         and change CMake to link `Vulkan::Headers` only. Clean,
         upstreamable, but requires forking `whisper-rs-sys` (we already
         fork llama).
      2. **Shim + `--as-needed` (now the front-runner):** provide our
         own definitions of the 3 bare symbols as lazy
         `dlopen`/`LoadLibrary` forwarders in a small C TU, and drop the
         loader from the link so nothing references it; `-Wl,--as-needed`
         then omits the `NEEDED` entry even if `-lvulkan` is still on the
         line. Requires **no ggml source edit and no `whisper-rs-sys`
         fork** — it scales to whatever bare-symbol set the linker
         reports. The `vkGetInstanceProcAddr` forwarder returning null
         when the loader is absent naturally yields zero devices → CPU
         fallback; the other two are only reachable after a device is
         created. Confirm the shared-vs-executable symbol resolution
         (no multiple-definition) in Phase 1.
- [ ] Task 0.3. **Confirm the `DynamicLoader` bootstrap (or
      `/DELAYLOAD:vulkan-1.dll` fallback) tolerates the pinned ggml on
      MSVC** — no unconditional Vulkan symbol reference during static
      init that would fault before the device probe. Test on the Windows
      box over SSH. *(Pending: needs the Windows host.)*
- [x] Task 0.4. **Verify CPU fallback end-to-end (Linux).** *(Done
      2026-07-12.)* Baseline: built the real GPU artifact (`fono-bench`
      w/ `accel-vulkan`) and confirmed the pre-fix `NEEDED` set is
      `{libstdc++, libvulkan.so.1, libgcc_s, libm, libc, ld-linux}`. The
      fixed-build fallback smoke is now done as part of Phase 1 — see
      Task 1.4. Windows loader-absent smoke stays on the Windows host
      (Task 0.3 / Phase 2.3).

**Phase 0 gate**: a documented, chosen mechanism per OS, prototyped far
enough to prove the NEEDED/import drops *and* the binary still uses the
GPU when present and CPU when not. **Met for Linux (2026-07-12):**
mechanism chosen and the linkage behaviour proven both by isolated
experiment and by the real artifact's `NEEDED`/bare-symbol set. Residual:
Windows-host tolerance (0.3) and the fixed-build fallback smoke (folded
into Phase 1.4 / Phase 2.3), both resource-blocked from this dev loop.

### Phase 1 — Linux GPU variant soft-load

**Chosen shape: #2 (shim + `--as-needed`), implemented 2026-07-12.** No
ggml source edit and no `whisper-rs-sys` fork were needed. The shim lives
at `crates/fono-core/src/vk_loader_shim.rs`, gated on `accel-vulkan` +
`target_os = "linux"` and registered from `crates/fono-core/src/lib.rs`
(relocated there from `fono-stt` in Task 1.6 — see below).
It defines the 3 bare symbols (`vkGetInstanceProcAddr`, `vkCmdCopyBuffer`,
`vkGetPhysicalDeviceFeatures2`) as lazy `dlopen("libvulkan.so.1")`
forwarders. `-Wl,--as-needed` is already the workspace default on Linux
GNU (`.cargo/config.toml:43`), so once our definitions satisfy ggml's
references, nothing pulls the loader and it drops out of `NEEDED` — no
new linker flag was required.

**Critical runtime finding (the reason a naive shim is not enough).**
The first shim returned a **null `PFN`** from `vkGetInstanceProcAddr`
when the loader was absent, on the theory that ggml would then enumerate
zero devices and use CPU. It does not: `ggml_vk_instance_init`
(`ggml-vulkan.cpp:5401-5403`) bootstraps its dynamic dispatcher with our
`vkGetInstanceProcAddr` and **immediately** calls
`vk::enumerateInstanceVersion()` *through the dispatcher*. A null `PFN`
there is a call through a null pointer → **SIGSEGV** (confirmed by gdb:
frame #0 `0x0`, frame #1 `vk::enumerateInstanceVersion`, frame #2
`ggml_vk_instance_init` at `:5403`). ggml *does* guard init —
`ggml_backend_vk_reg` (`ggml-vulkan.cpp:15091-15110`) wraps it in
`try { … } catch (vk::SystemError)` and returns a null registration
(⇒ zero Vulkan devices ⇒ CPU) — but that catch only fires for a thrown
C++ exception, never for a hardware fault.

**Fix:** when the loader is absent, `vkGetInstanceProcAddr` now returns a
non-null pointer to an error stub (`vk_stub_incompatible`) that reports
`VK_ERROR_INITIALIZATION_FAILED` (-3) for every requested entry point.
The first dispatched global call (`vkEnumerateInstanceVersion`) then
makes Vulkan-Hpp's `resultCheck` throw `vk::SystemError`, ggml catches
it, registers zero Vulkan devices, and inference falls back to CPU —
cleanly, no fault.

- [x] Task 1.1. **Apply the chosen patch.** *(Done 2026-07-12.)* Shim +
      module wiring added to `fono-stt`; no fork or `Cargo.toml` pin bump
      needed (shape #2). Shim is `#[rustfmt::skip]`-free and fmt-clean;
      compiles warning-free under `--features accel-vulkan`.
- [x] Task 1.2. **No longer hard-links libvulkan.** *(Done 2026-07-12,
      confirmed on the canonical artifact.)* `readelf -d` on the exact
      binary CI measures — `fono --profile release-slim
      --target x86_64-unknown-linux-gnu --features accel-vulkan` (ORT
      pinned via `scripts/fetch-onnxruntime.sh` as in CI) — shows
      `NEEDED = {libgcc_s, libm, libc, ld-linux}`, the 4-entry universal
      allowlist; `libvulkan.so.1` gone (libstdc++ is statically embedded
      in `release-slim`). Size 60,588,088 B (57 MiB), under the 72 MiB
      budget.
- [x] Task 1.3. **Update the gpu size-budget CI job.** *(Done
      2026-07-12.)* The `accel-vulkan` size-budget matrix row in
      `.github/workflows/ci.yml` now sets `extra_needed: ""` (was
      `libvulkan.so.1`), so the subset-check gate (`actual − allowlist`,
      `ci.yml:376`) now *asserts* the loader is absent — if it ever
      reappears in `NEEDED` the gate fails. Surrounding comments (rows
      226-234 and 355-359) refreshed to describe the soft-load. No
      `tests/check.sh` change needed — its `--size-budget` path only
      covers the `cpu` variant, whose allowlist is unchanged.
- [x] Task 1.4. **Launches on a Vulkan-less host and runs on CPU; uses
      the GPU when present.** *(Done 2026-07-12 — on both `fono-bench`
      and the canonical `fono` binary.)*
      - **Loader present:** `strace` shows the shim's
        `dlopen("/usr/lib64/libvulkan.so.1")` fires and 24 Vulkan ICDs
        enumerate; `fono-bench equivalence` PASSes (WER 0.0882) on the
        GPU with `libvulkan.so.1` absent from `NEEDED`. The canonical
        `fono doctor` reports `gpu variant … Vulkan: detected (Intel(R)
        Graphics (LNL), llvmpipe)`.
      - **Loader absent** (bind-mount a non-ELF file over
        `/usr/lib64/libvulkan.so.1` inside `unshare -rm`): pre-fix this
        segfaulted (exit 139); post-fix `fono-bench` exits 0 and
        transcribes on CPU with **identical** output (WER 0.0882), and
        the canonical `fono doctor` launches cleanly (exit 0) reporting
        `Vulkan: not available (libvulkan.so.1 not loadable…)`.
- [x] Task 1.5. **Linux `cpu` variant unaffected.** *(Done 2026-07-12.)*
      The shim and its module registration are gated behind
      `#[cfg(all(feature = "accel-vulkan", target_os = "linux"))]`, so the
      `cpu` build never compiles them. Confirmed empirically:
      `./tests/check.sh --size-budget` (cpu `release-slim`) = 21.36 MiB
      (≤ 25 MiB) with the 4-entry NEEDED allowlist clean — unchanged.
- [x] Task 1.6. **Relocated the shim to `fono-core` for robustness.**
      *(Done 2026-07-12.)* The shim first landed in `fono-stt`, which
      silently coupled its correctness to `fono-stt/accel-vulkan` being
      enabled. But `fono-polish` and `fono-assistant` link
      `llama-cpp-2/vulkan` — the *same* ggml, the *same* three bare
      symbols — independently of whisper. A polish-only GPU build
      (`fono-bench --features accel-polish-vulkan`) therefore linked
      `libvulkan` hard and would crash when the loader is absent. Moved
      `vk_loader_shim.rs` into `fono-core` (the shared crate both
      backends depend on), gated on `fono-core/accel-vulkan`, and made
      `fono-stt`/`fono-polish`/`fono-assistant`'s `accel-vulkan` each
      enable `fono-core/accel-vulkan`. Cargo feature unification now
      compiles the shim exactly once whenever *any* Vulkan backend is
      active. Verified: the polish-only build's NEEDED is the clean
      4-entry allowlist (was pulling `libvulkan`), the whisper-only build
      stays clean, and the GPU equivalence smoke still PASSes (WER
      0.0882).

### Phase 1 gate — all local CI gates green (2026-07-12)

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0 (the
  default CI gate; does not compile the shim since `accel-vulkan` is not
  default).
- `cargo clippy -p fono --features accel-vulkan --lib -- -D warnings` —
  clean; this *does* compile the shim. Fixed a latent, pre-existing
  `clippy::vec_init_then_push` in `crates/fono/src/daemon.rs`
  (`hardware_acceleration_summary`) that only fires under an accel
  feature (hence never caught by the default CI clippy) with a
  function-level `#[allow]` + rationale.
- `cargo test --workspace --tests --lib` — 36 suites, 0 failed.
- `./tests/check.sh --size-budget` — cpu variant passes.

### Phase 2 — Windows single Vulkan-with-fallback build

- [x] Task 2.1. **Add `accel-vulkan` to the Windows build.** Added
      `accel-vulkan` to the `windows-defaults` feature in
      `crates/fono/Cargo.toml`. **No `/DELAYLOAD` needed** — the
      cross-platform shim (`vk_loader_shim.rs`, extended with a
      `LoadLibraryA`/`GetProcAddress` `sys` module for `target_os =
      "windows"`) defines ggml's three bare Vulkan symbols itself, so
      MSVC satisfies them from our object and never pulls the import
      from `vulkan-1.lib`. Verified 2026-07-13: `dumpbin /DEPENDENTS
      fono.exe` shows **no `vulkan-1.dll`** in the PE import table. This
      is the exact Windows analogue of the Linux `--as-needed` result;
      the `/DELAYLOAD` hedge from Phase 0 is unnecessary.
- [x] Task 2.2. **Provision the Vulkan SDK / headers on the Windows
      build path.** Added a pinned LunarG SDK install step (v1.4.350.0,
      silent `--accept-licenses --default-answer --confirm-command
      install`, exports `VULKAN_SDK` + `Bin`) to both the CI `windows`
      job (`.github/workflows/ci.yml`) and the `release.yml` Windows
      row. Documented as a build prereq in `docs/build-windows.md`
      (gotcha #4 + "Vulkan single build"). Installed + verified on the
      bench (glslc, glslangValidator, `vulkan-1.lib`, headers all
      present). `scripts/win-remote.sh` inherits the box's system
      `VULKAN_SDK`, so no script change was required.
- [x] Task 2.3. **`fono.exe` launches and runs on CPU when
      `vulkan-1.dll` is absent, GPU when present.** Verified end-to-end
      on the Windows 10 bench 2026-07-13. Loader present: `doctor`
      reports `Vulkan: detected (Intel(R) HD Graphics 620)` and
      `fono-bench equivalence --model tiny --quick` transcribes on GPU
      (PASS, acc 0.0882). Loader absent (simulated with a bogus
      `vulkan-1.dll` in the exe dir — Windows searches the app dir
      before System32, so `LoadLibraryA` returns NULL, faithfully
      exercising the shim's error-stub path): the same transcription
      **exits 0, no crash**, CPU fallback, identical acc 0.0882. The
      error-stub fix (shared code) works identically on Windows. Also
      fixed the probe's hardcoded Linux loader name in
      `crates/fono-core/src/vulkan_probe.rs` to report `vulkan-1.dll`
      on Windows.
- [x] Task 2.4. **Self-update / variant plumbing stays single-artefact.**
      Confirmed the single-artefact asset naming
      (`fono-vX.Y.Z-x86_64.exe`, no variant suffix on Windows) is
      already correct and needs no logic change; updated the stale
      "CPU-only in v1" comments in `crates/fono-update/src/lib.rs` to
      describe the single Vulkan-with-fallback build.
- [x] Task 2.5. **Update the Windows size budget** to ~60 MiB. Added a
      2026-07-13 amendment to `docs/decisions/0022-binary-size-budget.md`
      (≤ 60 MiB enforced / ≤ 64 MiB hard cap, same family as Linux
      `gpu`) and a PE-import-allowlist rule: `vulkan-1.dll` must be
      **absent** from the import table (soft-loaded at runtime).
      `docs/build-windows.md` Phase 13 section updated (~15.7 MiB
      CPU-only → ~60 MiB Vulkan-with-fallback). The dumpbin/size CI
      assertion itself is deferred to Windows port Phase 14 (noted).

### Phase 3 — Docs, ADRs, gates

- [x] Task 3.1. **Amend `docs/decisions/0022-binary-size-budget.md`**
      — done as part of Task 2.5 (the 2026-07-13 amendment records both
      the ~60 MiB Windows budget and, via the Phase 1 amendments, the
      Linux-gpu NEEDED shrink to 4).
- [ ] Task 3.2. **Update `plans/2026-05-26-windows-port-v1.md`** Task 3.4
      / Phase 5.1 / Phase 14.3 to reference this plan as the superseding
      decision (done in the same session that files this plan — see the
      forward-pointers already added).
- [ ] Task 3.3. **README / `docs/build-windows.md` / `docs/install.md`**
      — Windows row describes a single GPU-accelerated `.exe` that falls
      back to CPU; note the larger download.
- [ ] Task 3.4. **CHANGELOG + ROADMAP** at release time (user-facing
      phrasing: "the Windows app now uses your GPU automatically when
      one is available, and the Linux GPU build no longer needs the
      Vulkan library just to start").
- [ ] Task 3.5. **Pre-commit + size gates green** on Linux
      (`cargo fmt --all --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace
      --tests --lib`, `./tests/check.sh --size-budget`).

## Verification criteria

- `fono-gpu` (Linux) NEEDED set is exactly the 4-entry universal
  allowlist — `libvulkan.so.1` gone.
- `fono-gpu` launches and transcribes on a host with no Vulkan loader.
- Linux `cpu` variant size + NEEDED unchanged (byte-identical).
- Windows `fono.exe` launches and transcribes on a host with
  `vulkan-1.dll` absent (renamed), and uses the GPU when present.
- Windows ships exactly one binary asset (`fono-vX.Y.Z-x86_64.exe`).
- All Linux gates green; Windows CI green.

## Risks and mitigations

1. **The pinned ggml doesn't cleanly support a dynamic loader.**
   Mitigation: Phase 0 spikes three mechanisms; the fork already exists
   as the place to carry a Windows/loader build patch.
2. **Delay-load faults if a Vulkan symbol is referenced during static
   init.** Mitigation: Phase 0.3 tests this explicitly; if it faults,
   fall back to the header-only-link approach that removes the import
   entirely.
3. **CPU fallback path is untested on Windows** because the maintainer
   rarely runs the Windows binary. Mitigation: the fallback is the same
   ggml zero-device path Linux CPU uses daily; add a CI smoke that runs
   `fono.exe` with `vulkan-1.dll` renamed to assert launch + CPU decode.
4. **Windows binary bloat surprises users.** Mitigation: it is a
   deliberate, documented trade (simplicity over size on Windows);
   README/install docs call out the larger download.
