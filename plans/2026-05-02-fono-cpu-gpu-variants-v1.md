# Fono — Two-binary release with GPU detection + upgrade UX

Date: 2026-05-02
Author: agent session continuing from v0.4.0

## Objective

Ship **two** release binaries instead of one:

- **`fono-vX.Y.Z-x86_64`** — CPU-only, ~18 MB, NEEDED allowlist of 4
  universal libs. The default download. Compact, runs everywhere.
- **`fono-gpu-vX.Y.Z-x86_64`** — Vulkan-enabled, ~60 MB, NEEDED
  allowlist gains `libvulkan.so.1`. Optional download for users on
  GPU-equipped desktops.

The CPU-only binary detects Vulkan-capable hardware at runtime. When
GPU is detected, fono offers the user the option to upgrade to the
GPU binary through three surfaces:

1. **First-run wizard** — tail-end prompt after `fono setup`.
2. **Tray icon** — menu item shown only when CPU-variant + GPU
   detected.
3. **CLI** — `fono update --variant gpu` (or `fono switch-variant
   gpu`); same direction reversible with `--variant cpu`.

## Why this exists

Local measurement (2026-05-02): `cargo build -p fono --profile
release-slim --features accel-vulkan` produces a **61 842 336-byte**
(59 MiB) binary. Vulkan adds **+42 MB**, ~20× the agent's initial
estimate. ggml-vulkan ships 150+ precompiled SPIR-V shaders (one per
kernel × dtype × variant) plus the Vulkan-Hpp dispatch infrastructure.
After fat-LTO + strip, that's still 42 MB of `.text`.

A single 60 MB binary defeats the "compact, runs on every Linux
distro" promise. A single 18 MB CPU-only binary defeats the "GPU
acceleration available" promise. The two-binary approach is honest
about the tradeoff: small for everyone, big for those who want GPU.

The GPU detection + upgrade UX bridges the gap so users who
unknowingly download the CPU build but have a capable GPU don't have
to know about the variant distinction up-front; fono offers it
on first run.

## Constraints

- **CPU variant** stays under 20 MiB with the strict 4-NEEDED-entry
  allowlist (existing v0.4.0 gate).
- **GPU variant** ≤ 64 MiB (sanity ceiling, not a tight gate); NEEDED
  set is the CPU allowlist + `libvulkan.so.1`.
- Both variants built from the same workspace, same source. The only
  build-time difference is `--features accel-vulkan`.
- No regressions on existing `fono update` flow (CPU-only users on
  same variant must keep working).
- Cross-variant switching is **opt-in only** — fono never silently
  downloads a different variant.

## Design

### Variant identification at runtime

Add a workspace-level `#[cfg]` constant:

```rust
// crates/fono-core/src/variant.rs (new)

pub const VARIANT: Variant = {
    #[cfg(feature = "accel-vulkan")]
    { Variant::Gpu }
    #[cfg(not(feature = "accel-vulkan"))]
    { Variant::Cpu }
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Variant { Cpu, Gpu }

impl Variant {
    pub fn label(self) -> &'static str {
        match self { Variant::Cpu => "cpu", Variant::Gpu => "gpu" }
    }
    pub fn release_asset_prefix(self) -> &'static str {
        match self { Variant::Cpu => "fono", Variant::Gpu => "fono-gpu" }
    }
}
```

Re-export from `fono-core` so every crate can read it.

### Vulkan detection in the CPU variant

Use **`ash`** (runtime-loaded Vulkan; no link-time dep). New module
`crates/fono-core/src/vulkan_probe.rs`:

```rust
pub struct VulkanProbe {
    pub available: bool,         // libvulkan.so.1 dlopen succeeded
    pub devices: Vec<String>,    // device names (empty if no GPUs)
}

pub fn probe() -> VulkanProbe {
    // 1. ash::Entry::load() — equivalent to dlopen("libvulkan.so.1")
    // 2. create a minimal Instance
    // 3. enumerate_physical_devices()
    // 4. return device names
    // Errors at any step → VulkanProbe { available: false, devices: vec![] }
}
```

`ash` is glibc-friendly and pure Rust; doesn't pull libvulkan into
NEEDED. The probe runs once at daemon startup (or on demand from the
wizard / tray).

Surface in `crates/fono/src/daemon.rs::hardware_acceleration_summary`
and `crates/fono/src/doctor.rs`. Three states:

- `Vulkan: active (Intel UHD Graphics 770, NVIDIA RTX 4070)` —
  GPU variant + libvulkan loaded + ≥1 device.
- `Vulkan: detected, GPU build available` — CPU variant + libvulkan
  loaded + ≥1 device. Suggests the upgrade.
- `Vulkan: not available` — no libvulkan or no devices.

### Release.yml matrix expansion

Add a `variant` axis:

```yaml
matrix:
  include:
    - target: x86_64-unknown-linux-gnu
      variant: cpu
      features: ""
      asset_prefix: fono
      os: ubuntu-22.04
    - target: x86_64-unknown-linux-gnu
      variant: gpu
      features: accel-vulkan
      asset_prefix: fono-gpu
      os: ubuntu-22.04
```

Build step adapts:

```yaml
- run: cargo build -p fono --profile release-slim --target ${{ matrix.target }} ${{ matrix.features != '' && format('--features {0}', matrix.features) || '' }}
```

Asset upload uses `${{ matrix.asset_prefix }}-${{ github.ref_name }}-${{
arch }}` so `fono-vX.Y.Z-x86_64` and `fono-gpu-vX.Y.Z-x86_64` end up
side by side in the release.

**Vulkan SDK on the runner** — ubuntu-22.04 ships `libvulkan-dev`
and `glslang-tools` in apt; add to the `Install Linux build deps`
step for the GPU matrix variant only.

### Packaging (.deb / .pkg.tar.zst / .txz / .lzm)

For v0.5.0 launch: **CPU variant gets full distro packaging; GPU
variant ships raw binary + .sha256 only.** Distro-packaged GPU
builds are a separate slice — most desktop users either grab the raw
GPU binary or rebuild the package themselves. Keeps the release
matrix manageable.

### CI gate split

`.github/workflows/ci.yml` needs two size-budget jobs:

- **`Binary size & deps audit (cpu)`** — current job, unchanged
  (≤ 20 MiB, 4-entry allowlist).
- **`Binary size & deps audit (gpu)`** — new job, builds with
  `--features accel-vulkan`, asserts ≤ 64 MiB and the 4 + libvulkan
  allowlist.

Use a job matrix to share the structure; `runs-on: ubuntu-22.04` for
both.

### Self-update with variant awareness

`fono-update` needs to:

1. Know which variant the running binary is (`fono_core::variant::VARIANT`).
2. By default, fetch the same variant's asset on a normal `fono update`
   (no surprises).
3. Accept `--variant cpu|gpu` to deliberately switch variants.
4. Verify the new binary's variant matches the user's intent (read its
   own NEEDED set or a magic string before swapping in — defensive
   check against asset-name confusion).

### CLI (simplified 2026-05-02)

`fono update` keeps its existing flags. **The variant decision is
internal**: every invocation probes Vulkan and picks `fono` or
`fono-gpu` automatically. Users do not pass a `--variant` flag —
the binary always self-aligns with the host's hardware:

- CPU build on a host without usable Vulkan → next update fetches
  `fono-vX.Y.Z-x86_64` (same variant, version bump only).
- CPU build on a host *with* usable Vulkan → next update fetches
  `fono-gpu-vX.Y.Z-x86_64` (variant switch + possible version bump).
- GPU build on a host without usable Vulkan (laptop undocked, eGPU
  unplugged, driver removed) → next update fetches the CPU asset.
- GPU build on a host with Vulkan → stays on `fono-gpu`.

Per [user feedback memory `feedback_centralize_decisions`]: one
decision in one place. No CLI flag, no wizard prompt, no
`gpu_upgrade_prompted` config knob.

### Tray UX (simplified 2026-05-02)

A single tray item — **"Update for GPU acceleration"** — shows up
only on a CPU-variant build with a usable Vulkan host. Click → the
existing `apply_update` path runs (which auto-picks `fono-gpu`
because the probe says so).

`fono-tray::TrayAction::UpdateForGpuAcceleration` + the
`GpuUpgradeProvider` callback type are added; the daemon's provider
returns `Some("Update for GPU acceleration")` only when both
conditions hold (CPU variant + probe `is_usable`). The action is
routed to the same `apply_update_via_tray` handler as `ApplyUpdate`
— no separate code path needed because the variant choice is now
folded into `fono_update::check`.

No "Switch to CPU build" item on the GPU variant: if the user is
on the GPU build and loses Vulkan capability, the next normal
update auto-switches them back. The tray doesn't need to surface
that explicitly.

### `fono_update::check` API (simplified 2026-05-02)

Signature change: `check(current_version, current_asset_prefix, channel)`.
The `current_asset_prefix` is `crate::variant::VARIANT.release_asset_prefix()`
in the binary call sites. Internally `check` calls
`desired_asset_prefix()` (which probes Vulkan) and treats a prefix
mismatch as an available update — that's how the
"Update for GPU acceleration" tray item lights up even at the same
version.

`asset_name_for(tag, prefix)` is parameterised on prefix.
`pick_release` and `fetch_latest` carry the prefix through.
`CPU_ASSET_PREFIX = "fono"` and `GPU_ASSET_PREFIX = "fono-gpu"`
constants live in `fono-update`.

### No wizard prompt; no config knob

Both removed from the original plan per simplification. The user's
hardware speaks for itself; we listen on every update.

## Phasing

This is too big for one PR. Slice it:

### Slice 1 — Release infrastructure (v0.5.0 launch)

- release.yml matrix expansion (cpu + gpu variants).
- ci.yml gate split (cpu + gpu size-budget jobs).
- New `fono-core::variant::Variant` constant.
- `fono doctor` and daemon log report which variant is running.
- Documentation: README install table mentions both variants;
  CHANGELOG; ROADMAP entry.

This ships the GPU binary as a **silent option** — users who know to
look for `fono-gpu-vX.Y.Z-x86_64` can grab it; everyone else stays
on CPU. Tag as **v0.5.0**.

### Slice 2 — Vulkan detection + doctor surfacing

- Add `ash` workspace dep (gated to a `vulkan-probe` feature, on by
  default — both CPU and GPU variants probe).
- New `crates/fono-core/src/vulkan_probe.rs`.
- Extend `hardware_acceleration_summary` to include the probe result
  with the three states above.
- Extend `fono doctor` "Compute backends" section.

Tag as **v0.5.1** (or roll into v0.5.0 if it's quick). No upgrade
prompt yet; just informational.

### Slice 3 — Upgrade UX

- `fono update --variant <cpu|gpu>` in `fono-update` + cli.rs.
- Tray `SwitchToGpuBuild` / `SwitchToCpuBuild` actions + menu items.
- Wizard prompt on Vulkan-detected CPU variant.
- Config knob `[update] gpu_upgrade_prompted`.

Tag as **v0.6.0** (UX-completing release).

## Verification

- Slice 1: release.yml builds both variants on tag push; sizes
  match (cpu ≤ 20 MiB, gpu ≤ 64 MiB); both NEEDED allowlists hold;
  draft release contains both binaries with .sha256 each.
- Slice 2: `fono doctor` on this Proxmox host (no GPU) reports
  "Vulkan: not available". On a GPU machine reports the device(s).
- Slice 3: end-to-end: download cpu binary on a GPU machine, run
  `fono setup`, accept upgrade, verify new binary runs with Vulkan
  active. Reverse: `fono update --variant cpu` from a GPU binary.

## Risks / open questions

1. **Self-update across variants** when the user is on a packaged
   install (`/usr/bin/fono`) — current update logic refuses to
   replace. Should cross-variant switch *also* refuse, or copy the
   GPU binary to `/usr/local/bin/fono-gpu` and rewrite a wrapper?
   Probably refuse with a helpful "your distro packaged this; use
   `fono-gpu` from the release page" message.
2. **`ash` crate size impact on the CPU variant** — `ash` is small
   pure-Rust bindings (~50 KB after LTO). Acceptable.
3. **Vulkan probe creates and destroys a Vulkan instance** — on
   some Mesa versions this can take 100–300 ms. Should run on
   daemon startup *off* the hot path, cache the result for the
   session.
4. **Distro-packaged GPU build** (slice 1 deliberately skips this).
   Is there demand for `fono-gpu` to ship as `.deb` / `.pkg.tar.zst`?
   Defer until a packager asks.

## Out of scope

- CUDA / ROCm release variants. Confirmed user-decision 2026-05-02:
  Vulkan is the supported GPU answer; vendor-specific stays
  build-from-source.
- aarch64 builds for either variant. Existing release.yml ships
  x86_64 only; aarch64 is a separate slice.
- Patching ggml-vulkan CMake to dlopen libvulkan (would let the GPU
  variant drop libvulkan from NEEDED). Long-term ideal; tracked as a
  follow-up that doesn't block this work.
