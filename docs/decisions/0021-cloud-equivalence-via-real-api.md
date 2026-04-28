# ADR 0021 — Cloud STT equivalence gate uses live Groq calls, not recorded mocks

Date: 2026-04-28
Status: Accepted

## Context

Wave 3 Slice B1 needed a cloud-side equivalence gate to keep the
streaming/batch contract honest across releases. Two designs were
considered:

1. **Recorded HTTP mocks.** Capture Groq responses once into committed
   JSON fixtures keyed by request body SHA, replay them in CI.
2. **Live Groq calls at release time** against a small fixture set,
   using a `GROQ_API_KEY` stored as a GitHub Actions repo secret.

## Decision

Adopt option 2. Wire the cloud-equivalence gate into
`.github/workflows/release.yml` as a `cloud-equivalence` job that runs
**before** the build matrix, and is auto-skipped on tags pushed
without `GROQ_API_KEY` in scope (forks, bootstrap tags) or with the
`-no-cloud-gate` suffix (operator escape hatch).

## Rationale

| Property | Mock | Live Groq |
|---|---|---|
| Offline / free | yes | $0.00 (well under free-tier daily cap) |
| Catches our regressions | yes | yes |
| Catches Groq schema changes | **no** | **yes** |
| Catches model deprecations | no | yes |
| Maintenance cost | recurring (refresh recordings) | none |

Mocks would have caught only one half of the breakage we care about:
our own code regressing the request shape. They would have missed the
half where Groq tweaks `verbose_json`, deprecates a model, or shifts
its rate-limit headers — which is exactly the failure mode our users
would hit first. Recorded fixtures also need ongoing maintenance every
time the upstream API drifts.

## Cost shape

The release-time gate runs against the existing 10-fixture multilingual
manifest at `tests/fixtures/equivalence/manifest.toml` (en × 4, ro × 3,
es × 1, fr × 1, zh × 1; ~110 audio-seconds total). Per release: 10
requests, ~110 audio-seconds. Groq's free tier (verified late 2025)
allows ~28,800 audio-seconds and ~43,200 requests per day — i.e. the
gate uses well under 0.5 % of either limit. Even a hypothetical 100×
tightening of the free tier leaves us with comfortable headroom.

A 250 ms inter-fixture sleep keeps us under the 30-req/min cap; HTTP
429 is treated as a hard fail with an explanatory message, never
retried.

## Fork-safety

GitHub does not expose repository secrets to workflow runs triggered
by pull requests from forks. The gate's `if:` condition checks
`secrets.GROQ_API_KEY != ''` and skips when empty, so:

- Forked PRs: cloud-equivalence skipped, build still runs (existing
  per-PR CI gate `ci.yml` is unchanged and remains free of cloud
  calls).
- Trusted-maintainer tags: cloud-equivalence runs with the secret;
  release blocked on failure.

This is enforced by GitHub at the platform level, not by our workflow
config — there is no path by which a malicious fork PR can read the
key.

## Manual override

`-no-cloud-gate` tag suffix (e.g. `v0.3.0-rc1-no-cloud-gate`) skips
cloud-equivalence even with the secret set. Use cases: Groq is down
and a release truly cannot wait; shipping a security fix that doesn't
touch the STT path; tag-and-rebuild loops during release iteration.

## Bootstrap

The committed baseline at `docs/bench/baseline-cloud-groq.json` must
be generated once locally (with the maintainer's API key) before the
gate is meaningful. The diff script
`.github/scripts/diff-cloud-bench.py` exits with code 2 and prints the
exact bootstrap command if the baseline is missing. See
`docs/dev/release-checklist.md`.

## Alternatives rejected

- **Recorded mocks.** See decision matrix above. Maintenance cost not
  justified.
- **Cloud calls on every PR.** Burns the free tier without
  proportionate benefit (most PRs don't touch the cloud STT path).
- **Both mocks and live calls.** Doubles infrastructure for marginal
  additional coverage. Defer; revisit only if live-call flakiness
  becomes a problem.
- **A weekly canary cron.** Worth doing as a follow-up to catch "Groq
  changed something between releases" within ≤ 7 days. Out of scope
  for closing Wave 3.

## Consequences

- Releases now depend on Groq being reachable. Acceptable: failure
  mode is "wait an hour, re-tag" and the override exists for genuine
  emergencies.
- Maintainers must regenerate the baseline whenever the manifest
  thresholds change, fixtures are refreshed, or Groq publishes a new
  Whisper model that materially changes accuracy. The diff-script
  output is unambiguous when this happens (verdict divergence list).
- The release workflow gains ~3 minutes of wall time on tag push for
  cargo build + 10 cloud round-trips.
