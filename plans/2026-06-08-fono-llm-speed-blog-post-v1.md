# THROWAWAY PLAN — Blog post: "How Fono makes local LLM dictation fast"

> Temporary working plan. Delete when the post ships. Lives in fono's `plans/`
> for convenience; the deliverable lives in `/mnt/nvme0n1p5/Work/bogdanr.github.io`.

## Goal

A blog post for bogdan.nimblex.net explaining the engineering behind Fono's
fast local LLM path — specifically the prompt-state cache + append-only prompt
re-ordering — with **real benchmark numbers**, a **fair head-to-head vs
ollama**, and a few **self-contained JS animations** to make it fun. Reader is
technical but not a llama.cpp insider. Voice: Bogdan's — first person, honest,
practical, a little wry, explains concepts as it goes (cf. washing-machine and
cloud-VM posts).

## Audience & tone checklist (from the existing posts)

- First person, conversational, short paragraphs.
- Teach the concept before using it ("here's what a KV cache is, in 90 seconds").
- Headers that read like steps / plain English.
- Numbered takeaways / bullet conclusions at the end.
- Inline code in backticks; raw HTML allowed (kramdown GFM) for `<script>`/`<canvas>`.
- One honest "and here's the dumb mistake we made" beat — fits the brand.
- Images under `assets/img/<slug>/`; JS can be inline or under `assets/js/`.

## Honesty guardrails (must not mislead)

- ollama is a general-purpose server built on the same llama.cpp; it is NOT
  "doing it wrong." Frame Fono's win as *purpose-built integration* (in-process,
  boot-warmed KV checkpoints, prompt laid out for maximal prefix reuse), not
  "ollama is bad." Any number we print must be reproducible and from the same
  machine + same model + same quant.
- Be explicit about what each benchmark measures (TTFB vs full latency) and why
  TTFB is the honest metric for a dictation assistant.
- Note the one place sampling diverged (turn-3 output mismatch = free-running to
  384 tokens on synthetic prompts), so we don't oversell.

## Phase A — Evidence / benchmarks

- [x] A1. Lock methodology: model = `gemma-4-e2b` (same Q4_0 GGUF for both),
      same box (dev-nimblex, 8 threads, ctx 4096), 6-turn conversation,
      metric = time-to-first-token per turn + full latency.
- [x] A2. ollama head-to-head: same GGUF imported as `fono-gemma-4-e2b-bench`,
      drove a scripted 6-turn `/api/chat` conversation, captured
      `prompt_eval_*`/`eval_*` + wall-clock TTFB. Ran the FAIR variant feeding
      ollama its own generated replies so its server-side prefix cache works at
      its best. Raw: `/tmp/ollama_bench_fair.py` output.
- [x] A3. Fono numbers: `assistant-conversation-cache` gives cached + uncached
      (the bug == the cache-OFF baseline) in one run.
- [x] A4. Curated three-way dataset: `/tmp/fono-runtime-prompt-cache/headtohead.json`.

## Phase A FINDINGS (2026-06-08)

Same model (`gemma-4-e2b`, Q4_0 GGUF), same box (dev-nimblex, 8 threads, ctx
4096), same 6-turn conversation. Metric = time-to-first-token (TTFB), ms/turn.

| Turn | Fono cached | Fono uncached (the bug) | ollama (warm; T1 cold) |
|---|---|---|---|
| 1 | 341 | 786 | 2649 (cold) |
| 2 | 641 | 1917 | 1397 |
| 3 | 383 | 2468 | 1322 |
| 4 | 509 | 3321 | 1352 |
| 5 | 491 | 4367 | 1317 |
| 6 | 375 | 4892 | 1521 |

Honest readings:
- **ollama is NOT naive.** Its server-side prefix cache keeps prefill flat
  (~0.8-1.2 s) instead of growing — it re-evaluates only new tokens. So the
  append-only insight is the universal hero, and any "ollama re-reads
  everything" claim would be false. Frame fairly: general server vs purpose-built.
- **Fono's win is real and measured:** warm TTFB ~0.34-0.64 s vs ollama's
  ~1.3-1.5 s (~2-4x), and cold turn-1 ~0.34 s (boot-warmed) vs ollama ~2.6 s
  (~8x). Drivers: instant KV-checkpoint restore (~20 ms) instead of re-prefilling
  the delta, in-process (no HTTP/second process), and boot warming.
- **The internal before/after** (Fono uncached vs cached) is the emotional core:
  the system-in-tail bug forced the "uncached" column on every turn after the
  first; the fix gives the flat "cached" column.
- Curated dataset for the animations: `/tmp/fono-runtime-prompt-cache/headtohead.json`.
- Caveat to keep: numbers are CPU-bound and a touch noisy (medians of 2-3 passes);
  one ollama outlier (5.6 s) was a contention spike, handled by median.

Open: optionally a cleaner re-run with more iterations / identical reply text for
the final charts; current data is solid enough to draft against.

## Phase B — Narrative

- [x] B1. Outline (locked, below).
      1. Hook: pressing a hotkey and waiting for a local model is death by a
         thousand milliseconds. We wanted instant.
      2. What actually costs time in a local LLM turn (prefill vs decode), in
         plain English, with an animation.
      3. KV cache 101: why the model re-reads the whole conversation every turn.
      4. The trick: snapshot the KV state and restore it (prompt-state cache).
      5. The dumb bug: we put the big unchanging system prompt at the *end*, so
         the cache missed every turn after the first (animation).
      6. The fix: append-only prompt ordering; system leads.
      7. The numbers: Fono cache off vs on; flat TTFB vs growing prefill.
      8. Head-to-head vs ollama (fair framing) + chart/animation.
      9. Takeaways + what's next (boot-warming, contention).
- [x] B2. Draft prose in Bogdan's voice — `_posts/2026-06-08-making-local-llm-dictation-fast.md`.

## Phase C — Animations (vanilla JS, no deps beyond the global jQuery already loaded)

- [x] C1. "Prefill vs restore" lane animation — done.
- [x] C2. "Where does the system prompt go" step-through (tail vs first, 3 turns,
      cached/read-again highlighting) — done.
- [x] C3. "The race" per-turn TTFB bars (cached vs uncached vs ollama) — done.
- [x] C4. Implemented as self-contained DOM + `requestAnimationFrame`,
      prefers-reduced-motion aware, IntersectionObserver lazy-start, mobile
      breakpoint. Lives in `assets/js/fono-llm-speed.js`; CSS inline in the post.

## Phase D — Assembly & verify

- [x] D1. Created `_posts/2026-06-08-making-local-llm-dictation-fast.md` with
      front matter (layout: post, Programming category, tags).
- [x] D2. No static images needed — all visuals are JS animations.
- [x] D3. Built locally (`RUBYOPT=-rlogger bundle exec jekyll build` — the
      `-rlogger` shim works around Ruby 4.0 dropping `logger` from default
      gems vs the pinned Jekyll 4.3.2; NOT a post issue). Verified: headings,
      code blocks, CSS, all 3 widget mounts, race data, and the resolved JS
      URL (`{{ site.url }}/assets/js/...` — fixed a missing-slash bug).
- [x] D4. Self-proofread pass done: voice consistent, every number traces to the
      dataset, fair ollama framing intact. Fixed one clarity nit ("pushed through
      the network" → "neural network" to avoid networking confusion). Rebuilt
      clean. Final human proofread still pending Bogdan's review.

## Phase E — Review loop

- [x] E1. Draft shown; Bogdan's round-1 notes incorporated (see Revision 2).
- [ ] E2. Final pass; remove this plan.

## Revision 2 (Bogdan's notes, 2026-06-08)

Addressed:
- **Framing:** Fono is a full local voice tool (F7 dictation + F8 assistant with
  tools), not "a dictation tool." Rewrote the intro.
- **Pipeline animation (`#fll-pipeline`):** shows the F7 and F8 paths, marks the
  model stage where caching applies, with a Cache ON/OFF toggle + "Run a turn".
- **Hardware section + animation (`#fll-bandwidth`):** new "why prefill and decode
  stress different hardware" section — prefill compute/SIMD-bound, decode
  memory-bandwidth-bound; animation contrasts parallel prefill vs per-token RAM
  streaming.
- **Tables:** conversation (per-turn TTFB cached vs uncached + restore + checkpoint),
  tool-count scaling, and the ollama head-to-head, all from the JSON artifacts.
- **Replicate-it-yourself section:** real `cargo build` + `assistant-conversation-cache`
  + `assistant-cache-scaling` commands and the ollama Modelfile/API recipe; the
  batch-size gotcha noted.
- **Memory honesty:** checkpoint sizes shown; the LRU bound called out (memory-for-
  latency trade).

My extra ideas offered to Bogdan (not yet built, awaiting his pick):
1. A "cost of a cache miss" slider (conversation length → prefill time, from real data).
2. A short "what we deliberately didn't do / limits" box (CPU-only, sampling noise,
   turn-3 output divergence) — partly covered by the caveat paragraph.
3. A diagram of the LRU eviction (how checkpoints are bounded). — DONE in Revision 3.
4. "Why not just keep one context alive per conversation?" sidebar (snapshot/restore
   vs persistent context; F7/F8 sharing one model).
5. A hero GIF/screen capture of Fono answering fast (needs a screen recording).

## Revision 3 (Bogdan's notes, 2026-06-08)

Addressed:
- **Hardcoded-domain bug:** the script tag used `{{ site.url }}/assets/js/...`,
  baking the absolute production domain into the HTML (breaks local preview).
  Changed to root-relative `/assets/js/fono-llm-speed.js`, matching every other
  post on the site (e.g. `/assets/js/BeltCalc.js`).
- **F7 vs F8 looked identical:** the pipeline animation (`#fll-pipeline`) is now
  data-driven by prompt mass. F8's assistant model carries a much larger cached
  prefix (system + tools + history ≈ 330 tok) than F7's fixed polish prompt
  (≈ 60 tok), so its green bar is ~4.5× longer and its cold-prefill cost dwarfs
  F7's (~2.9 s vs ~0.9 s cache-off; both ~0.5–0.6 s cache-on). The bar lengths
  and the per-lane clocks now make the "bigger job" visible.
- **LRU eviction diagram (`#fll-lru`):** new interactive widget under a new
  "Keeping it bounded" section. Shows the 8-checkpoint / 256 MB dual bound,
  a bytes meter, add/touch/reset controls, and LRU (amber, far left) → MRU
  (green, far right) ordering; pushing over either limit evicts oldest-first
  with a status line. Numbers match the real bounds in
  `crates/fono-assistant/src/llama_local.rs:216-217,258-260`.

Verify: `RUBYOPT=-rlogger bundle exec jekyll build` clean; all six widget mounts
present in rendered HTML (`fll-pipeline/-prefill-restore/-bandwidth/-prompt-layout/-lru/-race`);
JS resolves to the root-relative URL; editor JS syntax validation clean.

## Open questions for Bogdan (turn 1)

1. Angle: the honest "we found and fixed a subtle KV-cache bug, here are the
   receipts (incl. a fair ollama comparison)" story — yes?
2. ollama comparison framing: fair purpose-built-vs-general-server, not a
   takedown — agree?
3. Title direction / how spicy the "faster than ollama" headline should be.
