# ADR 0007 — Local-models build (musl-slim vs glibc-local-capable)

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from `CHANGELOG.md:242`, `docs/status.md` and plan
history, 2026-04-28).

## Context

The original "single static binary on musl" ambition collided with
`whisper.cpp`'s reliance on glibc-only symbols (`getauxval`, dlopen-style
SIMD probing in some build paths) and with `llama.cpp`'s GPU-accel
shims, which assume a glibc dynamic loader. Forcing musl meant either
gutting hardware acceleration or maintaining patches against upstream.
The team faced a choice: ship one slim cloud-only musl binary, ship
one fat glibc binary that does everything, or ship both flavours.

## Decision

Two release flavours:

- **`fono` (default, glibc-linked, local-models capable).** Bundles
  whisper.cpp + llama.cpp statically into a single ELF; runs out of
  the box on any reasonably modern glibc Linux. This is the recommended
  user download.
- **`fono-slim` (musl-static, cloud-only).** Built with
  `--no-default-features --features tray,cloud-all`. No bundled models;
  uses Groq / OpenAI / Cerebras / etc. Targets minimal Linux
  environments and containers where glibc is not available.

Documented in the install matrix on `index.html` and at
`README.md`'s build-flavour section.

## Consequences

- The "≤ 25 MB stripped, `ldd` reports not dynamic" verification gate
  from `docs/plans/2026-04-24-fono-design-v1.md` applies to the slim
  variant only; the local-capable build is glibc-linked by design.
- Distro packaging defaults to the local-capable build because
  whisper.cpp out of the box is the headline UX.
- Single-binary identity is preserved per flavour: each build still
  ships exactly one executable.
