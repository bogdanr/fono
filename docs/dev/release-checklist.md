# Release checklist

Pre-tag checklist for cutting a new Fono release.

## Per-release checklist

1. **Update `CHANGELOG.md`.** Promote the `## [Unreleased]` section
   to `## [X.Y.Z] — YYYY-MM-DD`. Confirm the body covers everything
   that landed since the previous tag. The release workflow extracts
   this section into the GitHub Release body.
2. **Update `ROADMAP.md`.** Move every item this release ships from
   the In progress / Planned sections into Shipped at the bottom,
   annotated `*vX.Y.Z, YYYY-MM-DD.*`.
3. **`cargo test` + `cargo clippy`** locally. CI also runs both, but
   catching it locally saves a release cycle.
4. **Tag** with `git tag -s vX.Y.Z` (or unsigned with `-a` if you
   don't have a PGP key configured), `git push origin vX.Y.Z`.
5. **Wait for the release workflow.** It runs the cloud equivalence
   gate first (see below), then the build matrix, then publishes the
   GitHub Release with artefacts.

## Cloud equivalence gate

On every tag push, `.github/workflows/release.yml` runs a
`cloud-equivalence` job that calls Groq's
`whisper-large-v3-turbo` against the multilingual fixture set at
`tests/fixtures/equivalence/manifest.toml` and diffs the verdicts
against the committed baseline at
`docs/bench/baseline-cloud-groq.json`.

The gate runs **before** the build matrix and blocks artefact
production on failure. Cost per release: ~110 audio-seconds, 10
requests; well under 0.5 % of Groq's free-tier daily cap.

### One-time bootstrap

Before the first release after wiring this gate, generate the
baseline locally with your Groq API key:

```sh
GROQ_API_KEY=gsk_... \
  cargo run --release -p fono-bench --features equivalence -- \
  equivalence \
    --stt groq \
    --output docs/bench/baseline-cloud-groq.json \
    --baseline --no-legend
```

Sanity-check the resulting JSON — every fixture should be `Pass`
unless a per-fixture threshold in the manifest needs revisiting.
Commit and push:

```sh
git add docs/bench/baseline-cloud-groq.json
git commit -s -m "docs(bench): bootstrap cloud-Groq equivalence baseline"
```

Subsequent releases will diff against this anchor. If the diff fails,
the script prints exactly which fixtures diverged.

### Regenerating the baseline

You only regenerate when an intentional change makes the old verdicts
obsolete:

- A manifest threshold was tightened/loosened.
- A fixture was refreshed or replaced.
- Groq published a new Whisper model that materially changed accuracy
  on our fixtures (and you've updated `whisper-large-v3-turbo` →
  `whisper-…-vN-turbo` in `crates/fono-stt/src/groq.rs`).

Same command as bootstrap; commit and tag.

### Override: skipping the cloud gate

When Groq is down or you're shipping a security fix that doesn't
touch the STT path, append `-no-cloud-gate` to the tag:

```sh
git tag -s v0.3.1-no-cloud-gate
git push origin v0.3.1-no-cloud-gate
```

The `cloud-equivalence` job's `if:` condition checks for this suffix
and skips. The release proceeds normally; the GitHub Release name
strips the suffix in the title (it's the operator's signal, not the
end-user's).

### Manual rerun after a Groq outage

If the cloud-equivalence job fails because Groq returned 5xx during
your tag push, simply re-trigger the workflow run from the **Actions**
tab. The gate is stateless; a fresh run with a healthy upstream will
pass.

## Rate-limit hard fail

If the harness hits HTTP 429 (Groq rate limit), it exits with code 3
and prints an explanatory message naming the fixture that triggered
the limit. Recovery: wait ~1 hour for the per-day window to reset, or
push the tag with `-no-cloud-gate` to bypass.

This should never happen in normal operation — the gate uses < 0.5 %
of the free-tier daily cap. If it does happen, suspect that a CI
runner is sharing the key with other workloads or that the workflow
got into a retry loop (it shouldn't; the harness is single-pass).

## Self-update verification matrix

For releases that touch `fono update` or the release-asset shape, run
the ten-scenario manual matrix at `docs/dev/update-qa.md` against a
prerelease tag (`vX.Y.Z-rc1`) before tagging the final.
