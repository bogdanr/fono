# ADR 0003 — License: GPL-3.0-only

## Status

Accepted 2026-04-24 (with known unresolved tension — see Consequences).

## Context

The project is open source and must be available to everybody. The maintainer
separately expressed the preference: *"I don't want commercial use without my
consent."* The license decision must reconcile those two statements as well as is
possible given the realities of OSI-approved licensing.

## Decision

**GPL-3.0-only.**

- `LICENSE` at the repo root contains the canonical GPL-3.0 text.
- Every Rust source file carries `// SPDX-License-Identifier: GPL-3.0-only` on
  line 1.
- All contributors must sign off each commit via the Developer Certificate of Origin
  (`git commit -s`). DCO is enforced by CI.
- No CLA at this time.

## Consequences

- All downstream distributions must remain GPL-3.0 (strong copyleft). Anyone who
  ships a modified Fono must ship their source under the same license.
- Fono can be packaged by any distro, vendored into other GPL-compatible projects,
  **and used commercially by end users** — GPL does **not** prohibit commercial use,
  contrary to the maintainer's initial expectation.
- **Unresolved tension**: the maintainer's stated goal of "no commercial use without
  my consent" is **not** achievable with GPL-3.0 alone. If that requirement
  resurfaces, the realistic options are:
  1. Accept GPL-3.0 as-is and allow commercial use (status quo).
  2. Switch to **AGPL-3.0** to close the SaaS loophole — still allows commercial
     use, but forces hosted/modified versions to publish source.
  3. Add a **dual-license path** (GPL-3.0 for the community + a commercial license
     on request). This requires a CLA so the maintainer can relicense.
  4. Adopt a non-OSI source-available license such as **BUSL** or **PolyForm
     Noncommercial**, at the cost of no longer being open source in the OSI sense.
- DCO-only sign-off (no CLA) means the project **cannot be relicensed** without 100%
  contributor consent. If a CLA becomes desirable later for option (3) above, it
  must be introduced *before* significant external contributions accrue — once a
  large external contributor base exists, relicensing becomes effectively
  impossible.

## Model license implications

Only Apache-2.0 / MIT / BSD-permissive weights are acceptable as *defaults* bundled
or downloaded by the first-run wizard. Llama-family and Gemma weights are
opt-in-only. See ADR 0004.
