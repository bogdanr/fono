# ADR 0018 — `ggml` link trick (`--allow-multiple-definition`)

## Status

Accepted 2026-04-27.

> **AMENDED 2026-06-24 — this is the steady state, not an interim kludge.**
> A spike (`plans/2026-06-23-shared-ggml-size-reclaim-spike-v1.md`)
> measured the duplicated-ggml reclaim available to the source-level
> shared-ggml replacement (ADR 0022 Task 1.2) at **≈ 0 MiB**: the shipped
> `cpu` artefact already contains a single ggml copy because
> `-ffunction-sections -fdata-sections` + `-Wl,--gc-sections` collect the
> loser copy's per-function sections, leaving exactly one definition of
> each `ggml_*` symbol. The earlier "~7 MiB" estimate was an archive-size
> inheritance that does not survive the link. **ADR 0022 will not supersede
> this ADR on size grounds; the link trick stays as the documented steady
> state.** See `docs/binary-size.md` §4 and the ADR 0022 amendment.

## Context

`whisper-rs-sys` and `llama-cpp-sys-2` each statically vendor their
own copy of `ggml`. Linking both into the same ELF — required for the
single-binary outcome of ADR 0005 — produces a `multiple definition of
ggml_*` link error from `ld.bfd` / `ld.gold`. Three approaches were
weighed:

- **Dynamic-link `llama.cpp`** as a private `.so`
  (`plans/closed/2026-04-27-llama-dynamic-link-sota-v1.md`) — gives up
  the single-binary identity, ships a companion file alongside.
- **Shared `ggml`** by patching both sys crates onto a single ggml
  build (`plans/closed/2026-04-27-shared-ggml-static-binary-v1.md`) —
  preserves single-binary but requires forking and tracking two sys
  crates' build systems.
- **Linker flag.** Pass `-Wl,--allow-multiple-definition` and let the
  linker keep one copy of each symbol while discarding the duplicate.

The last is the lightest touch and works because both copies of `ggml`
originate from the same `ggerganov/ggml` upstream and are pinned in
lockstep with their respective parent crates; they are ABI-compatible.

## Decision

Pass `-Wl,--allow-multiple-definition` for the
`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, and
`x86_64-pc-windows-gnu` targets in `.cargo/config.toml:21-28`. Both
crates' `ggml` copies link in; the GNU linker silently keeps one set
and discards the duplicate. macOS (`ld64`) and MSVC's `link.exe`
handle duplicate symbols differently and do not need the flag.

## Verification

- `nm target/release/fono | grep ' [Tt] ggml_init$'` returns exactly
  one entry (per `docs/status.md:286-289`).
- Smoke test `crates/fono/tests/local_backends_coexist.rs` constructs
  a `WhisperLocal` and a `LlamaLocal` in the same process to guard
  against runtime breakage on future sys-crate upgrades.
- Tier-1 integration: every CI workspace build links cleanly with
  the default features (`local-models`, `llama-local`).

## Trade-offs

- **ABI-compatibility burden.** The decision relies on both sys
  crates' vendored ggml staying ABI-compatible. If upstream ggml ever
  forks (e.g. one of the crates pins to a divergent fork), the linker
  will silently pick one set of symbols and the discarded set's
  callers will hit UB. Mitigation: the smoke test guards the obvious
  break; sys-crate upgrades are reviewed for ggml drift in CI.
- **Linker portability.** `lld` and `ld64` currently honour
  `--allow-multiple-definition` the same way as `bfd`/`gold`, but a
  hardened linker could withdraw the flag in the future.

## Rollback path

Plan H — `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` —
is the documented escape hatch. If the link trick ever fails on a
future linker, drop the duplicate ggml sources and link against a
single shared copy. The plan is preserved in `plans/closed/` for
exactly this reason.

## Surviving artefacts

- `.cargo/config.toml:21-28`
- `docs/status.md:276-310` ("Single-binary local STT + local LLM")
- `crates/fono/tests/local_backends_coexist.rs`
- `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` (rollback)
