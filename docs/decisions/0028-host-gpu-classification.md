# ADR 0028 — `HostGpu` classification from Vulkan `deviceType` + `shaderFloat16`

- **Status:** Accepted
- **Date:** 2026-05-25
- **Plan:** [`plans/2026-05-25-wizard-selection-heuristics-refresh-v5.md`](../bench/calibration/summary/plans/2026-05-25-wizard-selection-heuristics-refresh-v5.md)
- **Touches:** ADR 0027 (quantization ladder, amended), ADR 0026 (live preview as overlay style, supports the deletion of the live-mode RTF gate).

## Context

Before this change the wizard's affordability gate used a binary
`HardwareSnapshot::accelerated()` accessor: `true` on Apple Silicon or
when the host binary was built with a whisper.cpp acceleration
backend (`accel-cuda`, `accel-vulkan`, …), `false` otherwise. The
`true` arm applied a flat `4×` multiplier to the AVX2 batch RTF; the
`false` arm applied no multiplier.

The Phase 0 calibration (`docs/bench/calibration/summary/matrix.md`)
exposed two problems with that signal:

1. **Coarse on the high side.** A discrete RTX 4090 and an Intel UHD
   Graphics 620 both report "accelerated = true" when the binary is
   the Vulkan build, but the empirical wall-clock ratios are
   ~10× and ~1.0× respectively. The flat 4× over-promises on legacy
   iGPUs and under-promises on dGPUs.

2. **Coarse on the low side.** The `i7-7500u` Iris HD 620 reports
   `INTEGRATED_GPU` in Vulkan, but its Vulkan-on-CPU ratio on
   whisper-small is **0.82–1.10×** — not 4×. The reason is that
   ggml-vulkan's fp16 kernel path is gated on the device's
   `shaderFloat16` feature; without it, the backend silently falls
   through to a slower fp32 path that is competitive with AVX2 CPU
   inference. `shaderFloat16` is the single bit that discriminates
   "modern iGPU" from "legacy iGPU".

## Decision

Replace the binary `accelerated()` heuristic with a three-class
classifier:

```rust
pub enum HostGpu { None, Integrated, Discrete }

impl HostGpu {
    pub fn multiplier(self) -> f32 {
        match self {
            Self::None       => 1.0,
            Self::Integrated => 2.0,
            Self::Discrete   => 4.0,
        }
    }
}
```

Classification rule (applied to the union of physical devices the
Vulkan probe reports):

1. **`HostGpu::Discrete`** — any device with
   `VkPhysicalDeviceType == DISCRETE_GPU`.
2. **`HostGpu::Integrated`** — any device with
   `VkPhysicalDeviceType == INTEGRATED_GPU` **and**
   `VkPhysicalDeviceVulkan12Features.shaderFloat16 == VK_TRUE`.
3. **`HostGpu::None`** — everything else (legacy iGPU without fp16,
   `VIRTUAL_GPU`, `CPU` software rasteriser, no Vulkan loader, probe
   failure).

Apple Silicon is treated as `Integrated` unconditionally — Metal /
CoreML are always available on `macos / aarch64` and the empirical
ratio is in the 2× class. Intel Macs default to `None` (no Vulkan
on Apple's stack, no Metal benefit for whisper.cpp).

The multipliers `1× / 2× / 4×` are applied inside
`HardwareSnapshot::affords_model` as a final factor on the CPU AVX2
batch RTF (`rf × core_scale × isa_scale × host_gpu_mul`); the result
is gated against `BATCH_REALTIME_MIN = 2.0`.

## Empirical justification

From the five-host Phase 0 calibration matrix
(`docs/bench/calibration/summary/matrix.md`):

| Host | Reported `deviceType` | `shaderFloat16` | Vulkan/CPU ratio (whisper-small q5_1) | Mapped class |
|---|---|---|---|---|
| `i7-7500u` (HD 620, Skylake) | INTEGRATED_GPU | false | 0.82–1.10× | None |
| `i7-8550u` (UHD 620, Kaby Lake-R) | INTEGRATED_GPU | false | 0.93× | None |
| `i7-1255u` (Iris Xe, Alder Lake) | INTEGRATED_GPU | true | 1.40–2.50× | Integrated |
| `ultra7-258v` (Xe2, Lunar Lake) | INTEGRATED_GPU | true | 2.0–3.5× | Integrated |
| `ryzen-5950x` + RTX 4090 | DISCRETE_GPU | true | 5–10× | Discrete |

The classes are coarse on purpose. They reproduce the right
wizard-pick on every reference host (see the regression table in the
plan) without maintaining a PCI ID list.

## Forward compatibility

`shaderFloat16` is required by the Vulkan Roadmap 2022 profile and by
D3D12 Ultimate — every iGPU shipped from 2020+ has it, every new GPU
will. Pre-Xe Intel and pre-GCN3 AMD do not. This means the
`Integrated` class is self-tuning on future hardware without any
code change: when AMD or Intel ship a new iGPU, the Vulkan loader
already reports its capabilities; we read them, we get the class
right.

## Deliberate non-decisions

- **No runtime calibration probe.** A first-run benchmark that
  measures the real Vulkan/CPU ratio on a built-in clip is
  self-correcting but adds 5–10 s of first-run latency and a cache
  invalidation problem (driver updates, GPU hot-swaps). The
  `shaderFloat16` bit is the cheap discriminator; running anything
  was not on the table at this stage.
- **No PCI vendor / device ID table.** Precise per-device tuning at
  the cost of perpetual maintenance was rejected. `deviceType` +
  `shaderFloat16` is forward-compatible and self-tuning.
- **No user-visible multiplier.** The `1× / 2× / 4×` values are a
  gross internal estimate, not a promise. The wizard's hardware
  summary line shows qualitative text only ("Discrete GPU
  detected — Vulkan backend recommended" /
  "Modern integrated GPU detected — Vulkan backend recommended" /
  "Legacy / no GPU — CPU backend (AVX2 + FMA)").

## Risks and mitigations

- **Vulkan 1.2 instance requirement refused on ancient loaders.** The
  probe bumped its instance contract from `API_VERSION_1_0` to
  `API_VERSION_1_2` so the `shaderFloat16` feature query is in the
  core path. If `vkCreateInstance` rejects the 1.2 request, the
  probe returns `Outcome::NotAvailable { reason }` and the caller
  treats this as `HostGpu::None`. Any host that can't satisfy
  Vulkan 1.2 in 2026 is a legacy host that belongs in `None`
  anyway; the safe failure mode aligns with the right verdict.
- **Buggy driver under-reports `shaderFloat16`.** Worst case: a
  strong iGPU is under-classed to `None` and the wizard suggests a
  smaller model. The user can override via the wizard's manual
  picker or `[stt.local].quantization`. No silent over-promise.

## Consequences

The `accelerated()` accessor on `HardwareSnapshot` is deleted, the
`accel-*` feature flags on `fono-core` are kept only for build-matrix
plumbing (whisper.cpp linkage), and the wizard's affordability gate
is now driven by Vulkan facts the driver already reports.

See ADR 0027 (amended 2026-05-25) for the related decision to default
every model to `q8_0` regardless of `HostGpu`.
