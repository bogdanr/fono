# Contributing to Fono

Thanks for considering a contribution. Fono is GPL-3.0-only; by submitting a
patch you agree that your contribution is licensed under the same terms.

## Developer Certificate of Origin (DCO)

Every commit **must** be signed off with `git commit -s`. This adds a
`Signed-off-by: Your Name <you@example.com>` trailer to the commit message and
certifies that you have the right to contribute the change under the project's
license. CI enforces this on every PR; unsigned commits will be rejected.

The full Developer Certificate of Origin (v1.1) — <https://developercertificate.org/>:

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Code style

- `cargo fmt --all` must be clean. Configuration is in `rustfmt.toml`.
- `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- Every Rust source file starts with `// SPDX-License-Identifier: GPL-3.0-only`
  on line 1, followed by a blank line, then code. New files without this
  header will fail review.
- Lints are configured at the workspace level in the root `Cargo.toml`
  (`clippy::pedantic` + `clippy::nursery`, with a curated allow list).

## Tests

Run the whole suite with `cargo test --workspace --all-targets`. Target-
specific tests may be gated with `#[cfg]`; keep core logic platform-agnostic
where possible and isolate platform integration in the dedicated crates
(`fono-inject`, `fono-tray`, `fono-hotkey`).

## Adding a new STT or LLM provider backend

Both `fono-stt` and `fono-llm` are organised around a trait + backend module
pattern (see design plan Tasks 4.1 / 5.1). To add a provider:

1. Create a new module (e.g. `crates/fono-stt/src/backends/myprovider.rs`).
2. Implement the `SpeechToText` (or `TextFormatter`) trait.
3. Feature-gate the module in the crate's `Cargo.toml` under a `myprovider`
   feature so users building from source can compile out unused providers.
4. Register the backend in the crate's factory/dispatch function.
5. Add the provider to `docs/providers.md` with its HTTP endpoint, API key
   env-var name, supported models, and streaming capability.
6. Update `deny.toml` if the provider's SDK pulls in any new licenses (it
   shouldn't — stick to `reqwest` + `rustls`).

See `docs/plans/2026-04-24-fono-design-v1.md` (Tasks 4.3 and 5.3) for the
full provider matrix.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/): `feat:`,
`fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `perf:`, `build:`, `ci:`. The
first line should be ≤ 72 characters. Body (optional) explains *why*.

## Pull requests

- Keep PRs focused. Splitting a large change into a chain of smaller PRs is
  preferred over one monolithic diff.
- Make sure the PR description links the design-plan task(s) it implements
  (e.g. "Implements Task 4.2 from `docs/plans/2026-04-24-fono-design-v1.md`").
- CI (`cargo fmt`, `cargo clippy`, `cargo test`, `cargo-deny`, DCO check) must
  be green.
