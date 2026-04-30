# ADR 0022 — Binary size budget: single 20 MB static-musl binary

## Status

Accepted 2026-04-30. Supersedes ADR 0018 (`--allow-multiple-definition`)
once Phase 1 Task 1.2 of
`plans/2026-04-30-fono-single-binary-size-v1.md` lands the source-level
shared ggml.

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
`x86_64-unknown-linux-musl` `release-slim` artefact, **22 MiB** for the
`aarch64-unknown-linux-musl` artefact, with **all default features
enabled** (`tray + local-models + llama-local + interactive`). The
canonical ship build is:

```sh
cargo build -p fono --profile release-slim \
    --target x86_64-unknown-linux-musl
```

The same binary must:

- run `fono` locally on a graphical desktop (full pipeline + tray +
  overlay + text injection);
- run `fono serve` on a headless server (tray and overlay refuse to
  spawn at runtime when `DISPLAY` and `WAYLAND_DISPLAY` are both
  unset);
- run as a Wyoming / Fono-native client to a remote peer;
- have zero `NEEDED` shared libraries (`ldd` prints
  *"not a dynamic executable"*);
- contain exactly **one** copy of every `ggml_*` symbol
  (`nm $bin | grep -c '^[0-9a-f]\+ [Tt] ggml_init$'` returns `1`).

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
  --target x86_64-unknown-linux-musl` produces a `fono` ELF
  ≤ 20 971 520 bytes.
- `ldd target/.../release-slim/fono` prints *"not a dynamic
  executable"*.
- `nm target/.../release-slim/fono | grep -c '^[0-9a-f]\+ [Tt]
  ggml_init$'` prints `1`.
- The same binary, started with `DISPLAY` and `WAYLAND_DISPLAY` unset,
  brings up `fono serve` cleanly with tray and overlay refusing to
  spawn (`debug!` log lines, no errors).
- The same binary on a graphical desktop brings up tray + overlay +
  injection identically to today.
- `tests/check.sh --size-budget` passes in CI on every PR.
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
- **Static-libstdc++ + static-libgomp** on the musl target inflates
  the binary by ~1–2 MB compared to a glibc-dynamic build, but is the
  prerequisite for the no-shared-libs invariant. We accept the cost
  because the budget is calibrated against the static figure.

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
