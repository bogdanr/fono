# ADR 0019 — Platform scope for v0.x

## Status

Accepted 2026-04-28.

## Context

`docs/plans/2026-04-24-fono-design-v1.md:530-531` originally targeted
a "six-artifact" GitHub Release matrix covering Linux, macOS, and
Windows. After two release tags (`v0.1.0` … `v0.2.1`) the actual
release workflow at `.github/workflows/release.yml` ships **five
Linux-only** jobs (bare ELF + `.deb` + `.pkg.tar.zst` + `.txz` +
`.lzm`). No macOS or Windows artefacts have ever been published.

The user-base rationale is documented in `AGENTS.md`: Fono targets
Linux desktop users on light distros (NimbleX, Slackware-derived,
Arch, Debian) replacing Tambourine and OpenWhispr. The whole identity
of the project — single static binary, XDG paths, X11 + Wayland
focus handling — is Linux-shaped. Building macOS / Windows artefacts
would require either platform-specific CI runners (cost, slowness) or
cross-toolchains for `objc-foundation` / WASAPI bindings that are not
in scope for the current contributor pool.

## Decision

For the v0.x release line, ship Linux-only release artefacts. The
release matrix is:

1. Bare ELF (`fono` binary, glibc-linked, local-models capable).
2. `.deb` (Debian / Ubuntu / Mint).
3. `.pkg.tar.zst` (Arch / EndeavourOS / Manjaro).
4. `.txz` (Slackware / NimbleX).
5. `.lzm` (NimbleX live-CD module).

The musl-static slim variant from ADR 0007 ships as part of the bare
ELF flavour matrix (separate target triple) when packaging picks it up.

The original "six artifacts" target in
`docs/plans/2026-04-24-fono-design-v1.md:530-531` is amended to "five
Linux artifacts" for v0.x.

## Consequences

- macOS and Windows users build from source with the standard
  `cargo build --release` flow. The build is known to compile on
  macOS today (per the link-trick portability notes in ADR 0018) but
  is not tested in CI.
- Release CI cost stays small — five Linux jobs run on a single
  ubuntu-latest matrix.
- Cross-platform release jobs are revisited as a v1.0 concern, gated
  on demand from non-Linux users and a contributor willing to own
  the macOS / Windows packaging surface.

## Surviving artefacts

- `.github/workflows/release.yml` (the five-job matrix this ADR
  ratifies)
- `docs/plans/2026-04-24-fono-design-v1.md:530-531` (amended target)
- `AGENTS.md` (Linux-first user-base rationale)
