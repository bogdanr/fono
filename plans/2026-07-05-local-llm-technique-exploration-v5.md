# Local LLM Technique Exploration v5 — Follow-up Measurements Complete

Supersedes v4. Closes follow-ups #1 (Qwen concurrency retest) and #3 (EAGLE-3 measurement) from
v4's "Follow-ups Before Shipping" list. Follow-ups #2 (per-request gating design) and #4
(integration scoping) remain open — see revised list at the end.

## Follow-up 1 — Qwen3.6-35B-A3B concurrency retest (`-np 2`)

Re-run on the same laptop with more RAM headroom available at start (~6.6–18 GB free depending on
measurement point, no swap active — this environment's available memory keeps drifting because
the live-boot root is RAM-backed and other processes come and go; see note at the end).

| Config | EN tok/s | EN TTFT | RO tok/s | RO TTFT | CODE tok/s | CODE TTFT |
|---|---|---|---|---|---|---|
| `-np 1` baseline (from v4) | 21.28 | 1,025 ms | 17.58 | 949 ms | 18.52 | 1,787 ms |
| `-np 2` | 20.98 | 1,124 ms | **20.96** | 1,098 ms | 21.59 | 1,524 ms |

**Result: no regression, RO and CODE actually improved slightly (likely run-to-run noise rather
than a real effect of `-np 2`).** Combined with Gemma 4 26B-A4B's `-np 2` result from v4 (also
no regression), **both MoE candidates handle 2 concurrent slots cleanly at ctx 4096 on this
hardware.** Task 2.4 is now closed for both primary candidates. (Still not tested: genuinely
*simultaneous* in-flight generations from two clients — this harness sends requests sequentially,
so it validates "slot infrastructure doesn't hurt," not "two real users at once don't contend.")

## Follow-up 3 — EAGLE-3 speculator on Gemma 4 26B-A4B

Used the ready-made Apache-2.0 GGUF conversion (`williamliao/gemma-4-26B-A4B-it-speculator.eagle3-F16-GGUF`,
Q8_0 variant, ~992 MB) of RedHatAI's official EAGLE-3 speculator — no manual conversion needed,
confirming Phase 1's expectation that the checkpoint was directly usable.

| Config | EN tok/s | EN TTFT | RO tok/s | RO TTFT | CODE tok/s | CODE TTFT |
|---|---|---|---|---|---|---|
| no draft (baseline, from v4) | 24.33 | 683 ms | 18.74 | 894 ms | 20.32 | 1,126 ms |
| `-md <eagle3>` `--spec-type draft-eagle3` | 23.04 | 678 ms | **14.04** | 703 ms | **26.49** | 1,606 ms |

Draft acceptance rates tell the real story: **EN 30% / RO 7% / CODE 61%.**

- **CODE**: the clear win (+30% decode), consistent with Qwen's native MTP result — code is the
  single most speculation-friendly workload across every technique tried in this exploration.
- **EN**: a mild loss (−5%), acceptance too low (30%) for the verification overhead to pay for
  itself, though not badly so.
- **RO**: a **real regression (−25%)**, driven by a very low 7% acceptance rate — dramatically
  worse than Qwen's built-in MTP head on the same Romanian prompt (51% acceptance, near break-even
  in v4). This is the most important new finding of this follow-up: **EAGLE-3 speculators trained
  primarily on English hidden-state data generalize far worse to Romanian than a model's own
  native, jointly-trained MTP head.** A draft model that's a good match for the target's weights is
  not automatically a good match for the target's *language distribution* — those are separate
  axes, and only testing in the languages Fono actually needs (not just English) would have caught
  this.

## Revised Decision (updates v4 §Decision)

The v4 conclusion — that speculative decoding must be workload-gated, not universal — **now has a
second, independent confirmation and a sharper reason why**: it's not just "code speculates well,
prose doesn't," it's specifically **"draft-model speculation (EAGLE-3, standalone drafts) is far
more language-sensitive than native/trained-in speculation (MTP)."** Concretely, for a Fono default
that must handle Romanian and English both:

- **If shipping Gemma 4 26B-A4B**: enable `draft-eagle3` only for code/tool-call requests
  specifically (30–61% acceptance range, net win); leave it off for open-ended chat and
  **especially off for Romanian**, where it's a clear net loss.
- **If shipping Qwen3.6-35B-A3B**: `draft-mtp` is safe to leave on more broadly — worst case is a
  ~2–9% slowdown (EN/RO), not the ~25% regression EAGLE-3 showed on Gemma+RO — but code remains
  where it earns its keep most (+34.5%, v4).
- This nudges the two candidates' operating story apart: **Qwen3.6-35B-A3B's speculation is safer
  to default on; Gemma 4 26B-A4B's needs tighter, more conservative gating.** This is now a
  measured input to the "which MoE model becomes the shipped default" choice, not just a licence/
  publisher-trust consideration as in v4.

## Updated Verification Criteria — status

All v4 criteria remain ✅. Additionally now closed:
- ✅ Concurrency check completed for **both** MoE candidates (was: Gemma-only in v4).
- ✅ EAGLE-3 path measured for Gemma 4 26B-A4B (was: unmeasured in v4).

## Revised Follow-ups Before Shipping

1. ~~Re-run Qwen3.6 concurrency~~ — done above.
2. ~~Measure EAGLE-3 on Gemma 26B-A4B~~ — done above.
3. **Decide the per-request gating mechanism** for speculative decoding (workload metadata →
   `--spec-type` selection or per-request override at the API layer). Now informed by real
   numbers: gating needs to be language-aware, not just workload-aware, at least for
   EAGLE-3-backed configs.
4. **Scope the Fono-managed `llama-server` child-process integration**: model download/registry
   entry, config/tier wiring, process supervision, request-time `-np`/`--spec-type` overrides. This
   is the next engineering task — both candidate models, the safe universal flags, and the
   speculation caveats are now all measured and documented across v4+v5.
5. Consider one more measurement before finalizing the model choice: a Qwen3.6 EAGLE-3-style
   language-sensitivity check is moot (it has no separate EAGLE-3 draft — MTP is trained-in), but a
   *third* language beyond EN/RO would strengthen confidence that "MTP generalizes, standalone
   EAGLE-3 drafts don't" is a real pattern and not a two-point coincidence.
6. Clean up `../fono-tmp` once the decision is ratified (unchanged from v4).

## Environment note

This live-boot session's "available RAM" fluctuated considerably between measurement blocks (as
low as ~6.6 GB, as high as ~18 GB) with no swap active for most of this follow-up, for reasons
outside this exploration's control (root filesystem is RAM-backed by design on this live
environment; other foreground tools/processes come and go). All runs reported here completed
successfully with `EXIT: 0` and no OOM — but the throughput numbers should be read as "confirmed to
work under realistic memory pressure," not as guaranteed best-case figures. A persistent (non-live)
install with headroom fully available start-to-finish remains the recommended environment for any
number that needs to be defended precisely (e.g. in an ADR).

## Alternative Approaches

Unchanged from v4.
