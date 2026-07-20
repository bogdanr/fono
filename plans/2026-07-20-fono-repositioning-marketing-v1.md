# Fono Repositioning — "Talk to your computer"

## Objective

Retell what Fono is across every public surface. The product has outgrown its
"dictation tool" story: it is now an open-source, complete voice-AI stack —
STT, TTS, local LLM, wake word, speaker ID — in one small binary, serving
Wyoming and OpenAI/Ollama-compatible APIs, local by default with per-stage
cloud switching. The new positioning must pass the 3-second test for ordinary
users while giving developers/self-hosters their hook in the same fold. Honest
marketing: every claim is measurable and true of the current release — no
overselling, no underselling.

## The Canonical Copy Set (single source of truth)

All surfaces use these exact strings. Message discipline: same words everywhere.

### Category (2–3 words)

> **Local voice AI** (searchable category) / **the voice layer** (conceptual)

### Headline (site hero + README tagline)

> **Talk to your computer.**

### Subline

> Press a key and speak — Fono types into any app, answers as a voice
> assistant, or drives your coding agent. An open-source, complete voice-AI
> stack — speech-to-text, natural voices, a local LLM, wake word, speaker ID —
> in one small binary. Everything runs locally; every stage can switch to a
> cloud provider when you choose.

### GitHub "About" one-liner

> Talk to your computer — dictation, voice assistant, agent control.
> Open-source, complete voice-AI stack (STT · TTS · LLM · wake word ·
> speaker ID) in one small binary. Local or cloud, per stage. Wyoming +
> OpenAI/Ollama APIs.

### 10-second pitch (boilerplate paragraph)

> Fono gives your computer a voice interface: press a key and speak — it
> types, answers, or acts. It's the entire voice-AI stack in one 22 MB binary,
> local by default, cloud only when you opt in.

### 30-second pitch

> Fono makes voice a first-class input for your whole computer. Press a key
> and speak: your words land in any app, an assistant answers out loud, or
> your coding agent takes instructions. What makes it different is what's
> inside: speech recognition, natural voices, a local LLM, wake word, and
> speaker ID — the complete voice-AI stack — compiled into one small binary —
> 22 MB on CPU, 60 MB with Vulkan GPU acceleration that the installer picks
> automatically — with no Python, no Electron, no services to orchestrate.
> It's open source, and it doesn't keep that stack to itself: it serves
> Wyoming for Home Assistant and an OpenAI/Ollama-compatible API for
> everything else, so one Fono can be the voice backend for your whole
> network. Local-first for real; every stage can independently switch to a
> cloud provider when you choose.

### Techie hook (body copy / launch posts — never the headline)

> Think of it as the **SQLite of voice AI**: the whole stack, self-contained,
> one small file, no server farm to run. Point Home Assistant, Open WebUI, or
> your editor at it and it just answers.

### Six messaging pillars (every page repeats these, in this order)

1. **It does everything voice** — dictate, ask, command; STT / TTS / LLM /
   wake word / speaker ID.
2. **In one small binary** — ~22 MB CPU, ~60 MB with cross-vendor GPU
   acceleration (Vulkan: NVIDIA / AMD / Intel); no Electron, no Node,
   no Python.
3. **Local-first, actually** — nothing leaves the machine unless you opt in —
   and cloud-capable, per stage: swap just STT, or just TTS, to any of a
   dozen providers with one command.
4. **Fast, with receipts** — assistant's first spoken word in ~⅓ s on a
   laptop CPU, 2–4× ahead of Ollama on identical weights; the installer
   auto-picks the GPU build so acceleration costs you nothing.
5. **It serves, not just consumes** — Wyoming + OpenAI/Ollama APIs; one box
   voices the whole LAN.
6. **Open source, GPL-3.0** — no telemetry, no account, no strings.

### Style rules (honesty guardrails)

- Never write "high performance" — always the measured numbers (~⅓ s, 2–4×).
- Sizes are the concrete pair "~22 MB CPU / ~60 MB GPU", never a rounded
  bound like "under 70 MB".
- "Vulkan" never appears in headline or subline (pillar 2 and install docs
  only).
- No roadmap features in present tense; experimental platforms (macOS,
  Windows) stay qualified as they are today.
- "Open-source" appears as explicit text on fono.page (not implied by a
  badge, unlike GitHub).

## Implementation Plan

- [x] Task 1. **README hero rewrite.** Replace the tagline and subline at
      `README.md:8-11` with the canonical headline + subline. Keep the badge
      row, link row, and demo GIF (`README.md:26`) immediately after so the
      claim is proven visually within one scroll. Rationale: the README fold
      is the single highest-traffic surface; it must pass the 3-second test.
- [x] Task 2. **Add the developer hook to the README fold.** Insert one short
      paragraph after the hero (before `## Install`) carrying the SQLite
      framing and a compressed run of the six pillars. Rationale: developers
      and self-hosters are the amplifier audience; they must find their hook
      without scrolling past Install.
- [x] Task 3. **Rework "What you get" into the six pillars.** Reorder/merge
      the existing bullets at `README.md:62-72` to match the pillar order and
      wording (the content largely exists; this is alignment, not invention).
      Rationale: the pillars are the proof layer for the subline's claims.
- [x] Task 4. **ROADMAP header update.** Replace the stale framing at
      `ROADMAP.md:3-7` ("voice dictation tool for Linux") with the 10-second
      pitch; keep the epigraph line or replace with the new headline.
      Rationale: the roadmap is linked from the README and the site and
      currently contradicts the new story on both scope and platform.
- [x] Task 5. **Cargo.toml description.** Set the `fono` crate `description`
      to the GitHub About one-liner (trimmed to fit) so crates.io and
      packaging metadata inherit the positioning. Rationale: distribution
      surfaces should not tell an older story.
- [x] Task 6. **Packaging descriptions.** Update the description fields in
      `packaging/` (`.deb` control, Arch PKGBUILD-equivalents, SlackBuild
      `slack-desc`) to the 10-second pitch. Rationale: same message on every
      install path.
- [ ] Task 7. **GitHub repo settings (manual, owner action).** Set the About
      description to the canonical one-liner; align topics: `voice-ai`,
      `dictation`, `speech-to-text`, `text-to-speech`, `voice-assistant`,
      `wake-word`, `speaker-recognition`, `home-assistant`, `local-first`,
      `llm`, `vulkan`, `open-source`, `rust`. Rationale: About + topics drive
      GitHub search and link-preview text.
- [ ] Task 8. **fono.page hero (site repo / deployment).** Mirror the
      canonical headline + subline verbatim, with "open-source" explicit in
      text. Rationale: search visitors landing on the site don't carry
      GitHub's implicit context.
- [ ] Task 9. **Social preview image.** Regenerate the GitHub social card
      (and site OG image) with "Talk to your computer." over the logo so
      shared links carry the pitch. Rationale: most first impressions happen
      off-site in link cards.
- [ ] Task 10. **Release-notes boilerplate.** Adopt the 10-second pitch as the
      standing intro paragraph for future release notes and announcement
      posts. Rationale: repetition builds the category association.
- [x] Task 11. **Single commit.** Land all in-repo changes (Tasks 1–6) as one
      commit per project guidelines, DCO signed off, with a user-facing
      message, e.g. "Retell what Fono is: talk to your computer — a complete
      voice-AI stack in one binary". Run the pre-commit gate (fmt, clippy,
      tests) even though changes are docs/metadata-only, since Cargo.toml is
      touched.

## Verification Criteria

- A cold reader can state what Fono does after seeing only the H1 + subline
  (spot-check with 2–3 people unfamiliar with the project).
- Every quantitative claim (22 MB, 60 MB, ⅓ s, 2–4×, provider counts) matches
  the current release artifacts and existing documented benchmarks.
- README, ROADMAP, fono.page, GitHub About, Cargo.toml, and packaging
  descriptions carry identical core wording (grep for "Talk to your computer"
  and the one-liner across surfaces).
- No roadmap-only feature appears in present tense anywhere in the new copy.
- `cargo fmt/clippy/test` gate passes; no functional code changed.

## Potential Risks and Mitigations

1. **"Complete stack" reads as bloat to minimalists.**
   Mitigation: always pair the claim with the concrete binary sizes in the
   same sentence; the size is the rebuttal.
2. **"Talk to your computer" evokes Siri/Alexa clones.**
   Mitigation: the subline's first clause is dictation-into-any-app (which
   big-tech assistants don't do); pillar 3 (local-first) completes the
   differentiation.
3. **Numbers drift as releases evolve (sizes, latency).**
   Mitigation: Task 10's boilerplate lives in one place; add a checklist item
   to the release routine to re-verify the two size figures against the
   attached artifacts at tag time.
4. **Site and repo copy diverge over time.**
   Mitigation: this plan's "Canonical Copy Set" section is the single source
   of truth; any future copy change starts by editing it here (or a successor
   doc) and fanning out.

## Alternative Approaches

1. **Lead with "The SQLite of voice AI".** Maximally viral for the HN/dev
   audience but fails the 3-second test for ordinary users; kept as body-copy
   hook instead of headline.
2. **Keep "Dictate anywhere. Drive agents by voice."** Concrete but now
   undersells the assistant, the APIs, and the stack — the exact problem this
   plan fixes.
3. **"Voice ⟷ LLM engine" framing.** Accurate architecture description but
   "engine" reads as a library and "LLM" hides TTS/wake/ID; retained as an
   internal mental model only.
