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
