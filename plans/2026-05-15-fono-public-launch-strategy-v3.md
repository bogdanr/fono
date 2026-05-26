# Fono Public Launch Strategy (v3 — two-wave)

## What changed since v2

User correction: the voice assistant pipeline is real and shipping
(`ROADMAP.md:128-162`), but it is **not yet locally self-contained**.
Local Whisper works; local Qwen2.5 cleanup works but is sluggish on
CPU-only hardware (`docs/status.md:1872-1877`); local TTS still
points users at an external Wyoming-piper server because the
static-musl ship build can't yet pull in onnxruntime
(`CHANGELOG.md:459-462`). A Kokoro local + cloud parity plan is on
file (`plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`,
referenced from `CHANGELOG.md:152-153`).

Announcing "the first local-first voice assistant for Linux" while
the assistant pipeline requires cloud TTS by default is an
over-promise. HN, r/LocalLLaMA, and r/privacy commenters will read
the README, run `fono use tts local`, hit the Wyoming-piper-stub
message, and post the screenshot — and the credibility cost lands
permanently on the project, not on this release.

v3 restructures the strategy as **two waves with a deliberate gap
between them**, and tightens the v0.9 launch around what is
demonstrably ready today.

## Strategic Posture: Two Waves

### Wave 1 — "Linux voice dictation, done right" (v0.9.0, now)

Lead with **dictation**, the capability that is unambiguously ready,
fully local-capable, and faster than every cloud-coupled competitor
on the same hardware. Mention the assistant **honestly**: as a
working preview that today works best with cloud backends, with
local Whisper + local LLM available but cloud TTS recommended for
the spoken-reply experience. Don't bury it, don't lead with it.

Wave 1 captures:

- The "linux voice dictation" search traffic Fono is naturally
  positioned to win.
- Distro packaging pull-through (AUR, Nix, Fedora, Debian, Flathub,
  Homebrew Linux).
- The privacy-conscious dictation audience for whom local Whisper
  alone is the headline feature.
- An early-adopter cohort of assistant-curious users who'll surface
  bugs and feature requests that shape Wave 2.

What Wave 1 deliberately does **not** do: it does not pitch Fono as
a "voice assistant for Linux" in the title of any submission. The
assistant is documented, demoed, and reachable from the README —
but it is not the headline. The assistant headline is held in
reserve for Wave 2.

### Wave 2 — "The first fully-local voice assistant for Linux" (v1.0.0, later)

Triggered when **all three** of these are true:

1. **Local TTS is in-process** — Kokoro (or Piper via embedded
   onnxruntime, or another path the Kokoro parity plan resolves to).
   No Wyoming sidecar required; `fono use tts local` Just Works on
   the default binary. (`plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`.)
2. **Local assistant LLM produces an acceptable first-turn latency**
   on a representative mid-range machine. Concrete bar: a 1-2
   sentence question gets a first audible word back within 2.5 s on
   a 2022-era laptop with Vulkan (and within 5 s on CPU-only of the
   same era). Achievable with Qwen2.5-3B on Vulkan + Kokoro; needs
   measurement.
3. **A real end-to-end demo is recordable** showing F8 → spoken
   question → spoken reply with **zero network packets leaving the
   machine** (verifiable via `ss` / `iftop` overlay in the video).
   That demo is the Wave 2 launch artefact.

Wave 2's headline becomes available — and credible — when the demo
is true.

## Why two waves instead of one bigger launch

- **Two compounding attention events beat one larger one.** HN /
  Phoronix / press will cover both, especially with a strong
  technical-narrative arc ("we shipped dictation, then we shipped
  the first fully-local voice assistant"). Compounding outpaces
  single hits.
- **The dictation audience and the assistant audience overlap
  imperfectly.** Wave 1 wins the dictation cohort; Wave 2 wins the
  local-AI / privacy / accessibility cohort. Each gets a tailored
  story instead of a muddled dual one.
- **Credibility floor.** Launching only what's ready means every
  claim in the README survives a hostile commenter running every
  command in the install instructions. That floor compounds across
  both waves.
- **It buys time for the README and `fono.page` rebuild to land
  before the assistant headline is the load-bearing claim.** Tight
  scope on Wave 1 means we can ship the new website + README
  faster, refine them with real launch-traffic data, then re-use
  the polished surfaces for Wave 2.

## Positioning Thesis — Wave 1

Three sentences for every Wave 1 launch surface:

1. **"Press a hotkey, speak, see your words at the cursor — in any
   app, on any Linux desktop."** Dictation is the headline.
2. **"Local Whisper out of the box. Cloud providers one command
   away."** Local-by-default is the second message
   (`README.md:18-20`, `ROADMAP.md:92-112`).
3. **"One native Rust binary. ~18 MB. No Electron, no Python, no
   cloud account required."** Technical-shape message
   (`docs/status.md:484-526`).

A fourth supporting line, present but not headline:

> "Press F8 to ask a question — Fono streams a spoken reply through
> your speakers. Works with local Whisper + Qwen and a Wyoming-piper
> TTS server today; fully self-contained local TTS is in progress."

That sentence appears in the README's assistant section, in
`fono.page/features`, and in the second paragraph of the HN post.
It does **not** appear in any launch title.

## Positioning Thesis — Wave 2 (drafted now, held until v1.0)

1. **"Press F8. Ask anything. Hear the answer. Nothing leaves your
   machine."** The headline that's earned by then.
2. **"Three pipelines, one binary. STT, LLM, and TTS all run on your
   own CPU or GPU — or any cloud provider you choose."**
3. **"The first fully-local voice assistant for the Linux desktop."**

This thesis is drafted in the plan so the README rewrite (Phase 2)
can structure the assistant section to **evolve into** these lines
without a second rewrite at v1.0 — same anatomy, different verb
tense, different headline emphasis.

## Competitor Map

### Wave 1 (Dictation — active comparisons)

- **Tambourine** — Tauri + Python, webkit2gtk-4.1, python3.13, uv,
  libxdo, libayatana-appindicator3 (`AGENTS.md` external refs).
- **OpenWhispr** — Electron, ~100 MB runtime, hundreds of MB RAM.
- **Whispr Flow** — proprietary, paid, macOS-first.
- **nerd-dictation, numen** — open-source but project-shaped: no
  packaging, X11-only, no LLM cleanup.
- **Talon Voice** — heavyweight, command-grammar oriented.

### Wave 2 (Assistant — comparisons held until shipped)

- **Siri / Alexa / Google Assistant** — proprietary, cloud-only,
  no desktop integration.
- **ChatGPT voice mode** — cloud-only, phone-first.
- **Home Assistant + Wyoming voice** — appliance-shaped,
  config-heavy.
- **Open WebUI / LobeChat voice** — browser-locked.

Comparison pages for these competitors exist on `fono.page/compare/`
from Wave 1 (treating assistant as a present-tense capability with
cloud backends), but the framing softens: "Fono's voice assistant
today runs cloud LLM + cloud TTS by default; full local pipeline
in progress." Wave 2 rewrites those pages with the local headline.

## Implementation Plan — Wave 1 (now → v0.9.0)

### Phase 1 — Release engineering for Wave 1 launch

- [ ] Task 1.1. **Cut v0.9.0 as the Wave 1 launch release.** Anchor
  current `[Unreleased]` CHANGELOG window (HTTP instrumentation,
  TTS reliability fixes, wizard polish, multi-provider TTS,
  cascade-capped notifications — `CHANGELOG.md:8-220`). v0.9.0
  signals deliberate maturity without 1.0 SemVer commitment, and
  reserves v1.0.0 as the Wave 2 headline number.
- [ ] Task 1.2. **Regenerate benchmarks against v0.9.0** so the
  benchmarks page ships with current numbers.
- [ ] Task 1.3. **Opt-in install telemetry**: a one-time first-run
  prompt ("Help us count installs anonymously? y/N, default N"),
  POSTing `{version, variant, distro_id}` to `fono.page/_ping`.
  No persistent identifier, no IP retention beyond rate-limiting.
  Documented in `docs/privacy.md`. This is the only signal that
  shapes Wave 2 timing.
- [ ] Task 1.4. **`CONTRIBUTORS.md` and `GOVERNANCE.md`** at repo
  root. Distros check governance before adding to official repos.

### Phase 2 — README rewrite (dictation-led, assistant honest)

The current README opens with one line that doesn't mention the
assistant at all (`README.md:5`). The rewrite leads with dictation
but gives the assistant a labelled "preview" home with explicit
local/cloud status.

- [ ] Task 2.1. **Hero replacement.** Replace the single-line pitch
  with: line 1 — "Press a hotkey, speak — your words land at the
  cursor." Line 2 — "Native, local-first voice dictation for
  Linux. One Rust binary." Two demo GIFs immediately follow, each
  ≤ 8 MB so GitHub inlines them: dictation into an editor;
  provider hot-swap.
- [ ] Task 2.2. **"What is Fono?" section** with two paragraphs:
  - **Dictation (stable).** F7. Whisper transcribes locally,
    optional LLM cleanup, lands at your cursor. Works offline.
  - **Voice assistant (preview).** F8. Ask a question, hear the
    reply. Works today with local Whisper + cloud LLM + cloud
    TTS; local LLM available but slower on CPU-only; fully
    self-contained local pipeline is in progress (link to
    ROADMAP).
  Explicit "stable" / "preview" labels are the credibility
  preserver. A curious user lands on either capability with
  accurate expectations.
- [ ] Task 2.3. **Move the install matrix above the fold.** Split
  the current 9-row table into a 5-row primary table (Arch,
  Debian, NixOS, Slackware, one-liner) plus a collapsed
  `<details>` for GPU variant + self-installer specifics.
- [ ] Task 2.4. **"Why Fono?" comparison table — dictation only.**
  Rows: Fono / Tambourine / OpenWhispr / nerd-dictation / Wispr
  Flow. Columns: Linux native, Wayland, GPL-3.0, single binary,
  no telemetry, local Whisper, cloud optional, hot-swap providers,
  packaged in distro repos, price. Ruthlessly factual; one fudged
  checkmark on HN destroys credibility. The **assistant
  comparison table is deliberately held for Wave 2** — running a
  comparison table where Fono's "local TTS" column is asterisked
  invites the asterisk to dominate the conversation.
- [ ] Task 2.5. **"First run" section** demonstrates the dictation
  pipeline end-to-end. The assistant gets a sub-section labelled
  "Voice assistant (preview)" showing the cloud-default
  configuration, with an explicit "local TTS is in progress" note
  linking to the ROADMAP entry.
- [ ] Task 2.6. **"Switching providers"** keeps the current
  hot-swap demo (`README.md:48-87`) and adds one paragraph on
  swapping the assistant model (`fono use assistant <backend>`).
- [ ] Task 2.7. **Privacy section** expanded from two lines to a
  five-point list: local-by-default explained concretely; what
  each cloud backend sees, per provider; zero telemetry (with the
  opt-in install ping from Task 1.3 disclosed honestly); GPL-3.0
  link; reproducibility status.
- [ ] Task 2.8. **Benchmarks teaser** with one headline number,
  linking to the full benchmarks page on `fono.page`. From
  `docs/status.md:244-254`: "29-51× speedup on Vulkan vs CPU;
  sub-200 ms cold start; ~50 MB RAM idle."
- [ ] Task 2.9. **Embedded dictation audio sample** under `assets/`.
  One ≤ 200 KB Opus file: Fono's user-readable transcript output
  played back as TTS for accessibility. The assistant audio
  sample is held for Wave 2. Rationale: a voice tool you can
  *hear* converts better than one you can only see; but Wave 1's
  audio sample showcases what's stable, not what's preview.
- [ ] Task 2.10. **Curated badges row**: CI status, license,
  latest release (existing), star history graph, fono.page link.
- [ ] Task 2.11. **Trim documentation links** to three top-level
  references: Roadmap, Provider matrix, Troubleshooting.
- [ ] Task 2.12. **Contributing invitation** beyond pointing at
  CONTRIBUTING.md — three lines naming `good-first-issue`, the
  Matrix room (Wave 1 Phase 6), and the response SLA.

### Phase 3 — `fono.page` rebuild (Wave 1 launch minimum)

Audit-and-rebuild scope; if current site uses any JS framework,
rebuild as plain HTML/CSS or Zola (Rust-native, matches the
project's "no bloat" ethos).

- [ ] Task 3.1. **Audit current `fono.page` state.** Catalogue what
  exists, what's stale, what's missing. Decision gate on stack.
- [ ] Task 3.2. **Hero section** mirrors README hero exactly —
  same dictation-led message, same two demo videos (MP4 + WebM,
  muted autoplay loop, explicit controls — never autoplay audio),
  click-to-copy install command immediately below.
- [ ] Task 3.3. **Dictation feature column** with screenshot + 4-5
  bullets. **Assistant preview column** smaller and labelled
  "Preview — cloud TTS recommended today", with a "Subscribe for
  v1.0 — local voice assistant" email field underneath. That
  email list seeds Wave 2's launch audience.
- [ ] Task 3.4. **Provider matrix** as a polished table —
  STT / LLM / Assistant / TTS columns, one row per provider with
  green/grey badges. High-information-density page; functions as
  evergreen SEO bait for "groq whisper linux client" and similar.
- [ ] Task 3.5. **Benchmarks page** at `/benchmarks` fed from
  `docs/bench/` JSON. Three graphs: end-to-end latency, CPU vs
  Vulkan speedup, RAM-vs-feature-set. Refreshed on every release.
- [ ] Task 3.6. **Comparison pages** at `/compare/`:
  - `/compare/vs-tambourine`
  - `/compare/vs-openwhispr`
  - `/compare/vs-wispr-flow`
  - `/compare/vs-nerd-dictation`
  Each captures search traffic for "[competitor] alternative
  linux". **Assistant-category comparison pages are held for
  Wave 2.**
- [ ] Task 3.7. **Install page** at `/install` with per-distro
  deep instructions. Copy-pastable; no JS expansion walls.
- [ ] Task 3.8. **Privacy page** at `/privacy` mirroring and
  expanding the README privacy section.
- [ ] Task 3.9. **OpenGraph + Twitter Card metadata** on every page
  with per-page preview images (1200×630).
- [ ] Task 3.10. **Sitemap + `robots.txt` + submission** to Google
  Search Console and Bing Webmaster Tools.

**Held for Wave 2 polish (do not block v0.9.0):**

- [ ] Task 3.11. Live audio demo "Try it" button (held — the demo
  worth recording is the local-only one).
- [ ] Task 3.12. Use-cases / recipes page (held — adds
  assistant-category use cases).
- [ ] Task 3.13. Blog seed posts (held to land alongside Wave 2
  launch).

### Phase 4 — Soft launch (Wave 1, week 0)

- [ ] Task 4.1. **Tag v0.9.0, publish GitHub release** with full
  CHANGELOG body, all distro packages, both CPU/GPU variants.
- [ ] Task 4.2. **Package-repo submissions** in priority order:
  - [ ] AUR `fono-bin` mirroring `.pkg.tar.zst`.
  - [ ] Nix flake into `nixpkgs` proper.
  - [ ] Fedora COPR as stepping stone to Fedora official.
  - [ ] Debian `mentors.debian.net` + ITP bug.
  - [ ] Flathub (static binary makes the manifest trivial).
  - [ ] Homebrew Linux formula.
  Every distro adoption is a permanent, compounding discovery
  surface.
- [ ] Task 4.3. **Announce to immediate network first.** Personal
  network, technical friends, work Slack/Discord. The first 48
  hours are friends-of-author; engineer that signal deliberately.

### Phase 5 — Loud launch (Wave 1, week 1)

- [ ] Task 5.1. **Hacker News "Show HN" post.** Title: *"Show HN:
  Fono — native voice dictation for Linux in one Rust binary"*.
  Body: dictation-led thesis, two GIFs, install line, repo
  link. Mentions the assistant in the **third** paragraph with an
  honest "preview, cloud TTS today, local in progress" framing.
  Submit Tue-Thu morning US Eastern. Be online 6+ hours after
  posting. Pre-write a maintainer top-comment preempting the
  "local by default, GPL-3.0, no telemetry" thread.
- [ ] Task 5.2. **Lobsters submission** with tags `linux`,
  `programming`, `audio`.
- [ ] Task 5.3. **Reddit, staggered over 3-5 days**, dictation-
  led angles per subreddit:
  - r/linux: "native, no Electron".
  - r/rust: the `--allow-multiple-definition` whisper.cpp +
    llama.cpp coexistence trick (`docs/status.md:1825-1842`).
  - r/i3wm, r/swaywm, r/KDE, r/gnome: tiling-WM / Wayland /
    KDE-Plasma / GNOME-friendly angles.
  - r/commandline: the CLI surface.
  - r/voicedictation: head-to-head against Whispr Flow.
  - r/privacy + r/StallmanWasRight: GPL-3.0 dictation tool
    with zero telemetry.
  - **r/LocalLLaMA and r/selfhosted are held for Wave 2** —
    those subs are the assistant-pipeline audience; spending the
    attention there on a dictation post wastes the channel.
- [ ] Task 5.4. **Mastodon + Bluesky + X threads** with the
  dictation demo GIF. Tags: `#Linux`, `#OpenSource`, `#Rust`,
  `#VoiceTyping`, `#FOSS`, `#PrivacyTools`.
- [ ] Task 5.5. **Long-form launch blog post** on `fono.page/blog`
  (if seeded by then; otherwise dev.to mirror). Topic: "Building
  native Linux voice dictation: lessons from replacing Electron
  with one Rust binary". Cover the linker trick, the CPU/GPU
  variant split, the static-musl pivot.

### Phase 6 — Wave 1 press + sustaining (weeks 2-4)

- [ ] Task 6.1. **Linux tech press**, priority order: Phoronix
  (GPU variant story is Phoronix-shaped), OMG! Ubuntu, It's FOSS,
  LWN.net, Linux Magazine, Linux Format. ≤ 150-word pitches with
  a single GIF link. **AI/ML press is held for Wave 2** —
  pitching The Decoder / MarkTechPost on a cloud-coupled
  assistant is wasted; the local-pipeline story is what those
  outlets cover.
- [ ] Task 6.2. **YouTube creator outreach**: Brodie Robertson,
  DistroTube, The Linux Experiment, Mental Outlaw, Chris Titus
  Tech. Dictation-led pitches.
- [ ] Task 6.3. **Podcast outreach**: Linux Unplugged, Late Night
  Linux, Coder Radio.
- [ ] Task 6.4. **Awesome lists**: `awesome-rust`, `awesome-linux`,
  `awesome-selfhosted`, `awesome-cli-apps`, `awesome-local-first`.
  **`awesome-local-ai` and `awesome-llm-apps` are held for Wave 2.**
- [ ] Task 6.5. **Community channel**: Matrix room
  (`#fono:matrix.org`). Linked from README + website + `fono doctor`.
- [ ] Task 6.6. **Pre-baked `good-first-issue` queue** of five
  scoped tasks (wake-word, Wayland portal hotkeys, additional
  TTS backend, curated-list language, distro packaging recipe).
- [ ] Task 6.7. **48-hour issue-response SLA.** Acknowledge every
  issue within 48 hours.
- [ ] Task 6.8. **macOS + Windows interest list** on `fono.page`.
  Public commitment with a target quarter expands audience and
  produces a future attention wave (`ROADMAP.md:82-84`).

## Implementation Plan — Bridge (between waves)

This phase runs in parallel with Wave 1 sustaining and is what
makes Wave 2 launchable. Roughly 2-4 months of work depending on
the Kokoro path resolution.

- [ ] Task B.1. **Resolve the Kokoro local + cloud parity plan**
  (`plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`). Ship
  `tts.local` backed by Kokoro (or Piper via embedded
  onnxruntime, or whichever path the plan resolves to) so
  `fono use tts local` works on the default binary with no
  Wyoming sidecar.
- [ ] Task B.2. **Llama Vulkan prewarm** (the follow-up flagged at
  `docs/status.md:256-259`). First assistant turn after session
  start should not pay the equivalent pipeline-compile cost the
  Whisper Vulkan prewarm now avoids
  (`docs/status.md:220-254`). Bench gate: first-turn
  time-to-first-audible-word on Vulkan ≤ 2.5 s on a 2022-era
  laptop.
- [ ] Task B.3. **Local-assistant benchmark fixture set** in
  `docs/bench/` covering the local Whisper → local Qwen2.5 →
  local Kokoro chain, with stable numbers per hardware tier.
  Wave 2's headline ("zero packets leave the machine, here are
  the latencies") depends on these.
- [ ] Task B.4. **Wave 2 acceptance gate.** A live demo, recorded
  with `ss -tunap` / `iftop` running in an overlay panel,
  showing F8 → spoken question → spoken reply with **zero
  external network packets**. The video itself is the Wave 2
  launch artefact; not having it kills the Wave 2 thesis.
- [ ] Task B.5. **Wake-word activation** (`ROADMAP.md:43-51`).
  Optional for Wave 2 but enormously amplifies the headline:
  "say the word, get an answer, never touch the keyboard, never
  leave the machine." If shippable in the Wave 2 window, ship it.
- [ ] Task B.6. **Re-record the assistant demo** for README + site
  against the local-only pipeline once Tasks B.1-B.3 land.

## Implementation Plan — Wave 2 (v1.0.0, when bridge tasks complete)

- [ ] Task W2.1. **Cut v1.0.0** as the Wave 2 launch release.
  v1.0 commits to SemVer guarantees on the public surfaces
  (config schema, CLI flags, IPC protocol); document the
  guarantees explicitly in `docs/stability.md`.
- [ ] Task W2.2. **README assistant section graduates to
  co-headline.** Same anatomy designed in Wave 1 Phase 2, now
  with "stable / local-capable" labels and the local-only demo
  GIF replacing the cloud-default one.
- [ ] Task W2.3. **`fono.page` assistant feature column promoted**
  to equal weight with dictation; held comparison pages
  (`/compare/vs-chatgpt-voice`, `/compare/vs-home-assistant-voice`,
  `/compare/vs-siri-alternatives`) ship; live-audio "Try it"
  demo (Wave 1 Task 3.11) ships against the local pipeline.
- [ ] Task W2.4. **Hacker News "Show HN" post #2.** Title:
  *"Show HN: Fono v1.0 — the first fully-local voice assistant
  for Linux"*. Body: the local-only demo video, the latency
  numbers from Task B.3, install line, repo link. The Task B.4
  network-trace video is the leading visual.
- [ ] Task W2.5. **r/LocalLLaMA, r/selfhosted, r/privacy posts**
  (held from Wave 1). r/LocalLLaMA in particular is the
  highest-fit audience this project will ever address; the
  post is the launch's center of gravity, not HN.
- [ ] Task W2.6. **AI/ML press outreach** (held from Wave 1): The
  Decoder, MarkTechPost, Towards Data Science, /r/MachineLearning.
  Angle: "first fully-local alternative to ChatGPT voice mode on
  desktop Linux".
- [ ] Task W2.7. **Awesome-list expansions**: `awesome-local-ai`,
  `awesome-llm-apps`, `awesome-llmops`.
- [ ] Task W2.8. **Long-form blog post**: "Why we waited eight
  months to call Fono a voice assistant." Honest narrative about
  Wave 1's deliberate restraint. Rationale: this is a story
  technical audiences respect, and it cements the project's
  credibility for the long run.

## Verification Criteria

### Wave 1 (v0.9.0, weeks 0-12)

- README rewritten with dictation hero, "What is Fono?" dual section
  with stable/preview labels, dictation comparison table, dictation
  audio sample, opt-in telemetry endpoint live.
- `fono.page` rebuilt with hero, provider matrix, benchmarks, four
  comparison pages, install, privacy.
- v0.9.0 tagged.
- Week 1: HN > 50 points; one of Phoronix / OMG! Ubuntu / It's FOSS
  scheduled; > 500 stars; ≥ 3 targeted subreddits hit /top weekly.
- Week 4: AUR live, Nix in `nixpkgs` PR review, Fedora COPR live,
  Flathub manifest submitted, ≥ 1 YouTube video published.
- Week 12: ≥ 3 external contributors with merged PRs, ≥ 1 official
  distro inclusion, > 2000 stars, monthly install count visible.

### Bridge (week 12 → Wave 2)

- Task B.4 acceptance gate met: live local-only assistant demo
  recordable with zero outbound packets.
- Task B.1 + B.2 produce a documented latency profile per
  hardware tier.

### Wave 2 (v1.0.0)

- v1.0 tagged with `docs/stability.md` SemVer commitments.
- README + `fono.page` assistant section graduates to co-headline.
- Wave 2 HN post + r/LocalLLaMA post hit /top within 48 hours.
- Three additional distro-repo inclusions (Debian unstable, Fedora
  official, Homebrew main).
- > 5000 stars cumulative; > 10 external contributors with merged
  PRs.

## Potential Risks and Mitigations

1. **Wave 1 fails to gather enough attention to justify a Wave 2.**
   *Mitigation*: even at modest Wave 1 reach, the distro packaging
   pull-through is permanent and compounds. Wave 2 has its own
   announcement vehicles regardless of Wave 1 numbers.

2. **Bridge phase drags past 4 months and Wave 1 momentum stalls.**
   *Mitigation*: Phase 7-equivalent sustaining work (2-3 week
   release cadence on dictation-side improvements + monthly
   retrospectives in `docs/status.md`) keeps repo signal alive.
   The Kokoro plan exists in writing; resourcing is the variable.

3. **A competitor ships a fully-local Linux voice assistant during
   the bridge phase.** *Mitigation*: this is the genuine downside
   of the two-wave approach vs a one-shot launch. Counter-argument:
   no current open-source competitor is close (the comparison map
   above is exhaustive), and shipping a half-baked Wave 2 today
   would lose to the same hypothetical competitor anyway. The
   right defence is shipping Wave 2 well, not shipping it early.

4. **Wave 1 commenters dig into the assistant preview and the
   credibility loss lands anyway.** *Mitigation*: the preview is
   labelled honestly in the README (Task 2.2). The honest preview
   framing is itself credibility-building — "this is shipping, it
   works, here's where it doesn't yet" reads as serious engineering.
   The dishonest framing is the one to avoid.

5. **Provider model deprecation mid-Wave-1.** Three retirements
   landed during `[Unreleased]` (`CHANGELOG.md:186-258`).
   *Mitigation*: re-record demos against v0.9.0 the day of
   announcement; lead Wave 1 demos with the local dictation
   pipeline, which has zero provider dependency.

6. **Maintainer bandwidth collapse**. *Mitigation*: pre-baked
   `good-first-issue` queue (Phase 6 Task 6.6); explicit
   response-SLA in CONTRIBUTING.md; canned responses for the
   five most-likely FAQs.

7. **GPL-3.0 friction with corporate users.** *Mitigation*:
   deliberate choice per `AGENTS.md`; do not waver.

8. **Wave 1 README "preview" framing reads as "not ready" and
   suppresses overall interest in Fono.** *Mitigation*: this is
   why Wave 1's headline is dictation, which is unambiguously
   ready. The preview label sits on the assistant capability
   alone; it does not contaminate the dictation message. A
   reader who only needs dictation gets a clean stable pitch;
   a reader curious about the assistant gets honest preview
   framing and an invitation to subscribe for v1.0.

## Alternative Approaches

1. **One-shot dual launch (v2's recommendation, now rejected).**
   Lead with both dictation and assistant simultaneously at v0.9.
   Trade-off: bigger single attention event but vulnerable to the
   credibility cost of the cloud-coupled assistant claim. Rejected
   per user feedback.

2. **Wait for the full local stack before any launch.** Skip v0.9
   entirely, ship v1.0 with both capabilities ready, single big
   launch. Trade-off: gives up months of compounding distro
   packaging and audience growth, defers all launch-traffic
   learning until the highest-stakes moment. Worse than two waves.

3. **Three-wave launch.** v0.9 dictation; v1.0 cloud assistant
   polish; v1.1 local assistant. Trade-off: dilutes attention
   across three events instead of two; cloud-assistant polish is
   not big enough on its own to carry a dedicated wave. Worse
   than two waves.

4. **Niche-first Wave 1.** Skip HN/general-Linux entirely; spend
   Wave 1 inside the Home Assistant + Wyoming community (Wyoming
   server mode already ships per `ROADMAP.md:226-241`), build a
   fanatical niche cohort, then broaden at Wave 2. Trade-off:
   smaller absolute reach but produces a foundational audience
   that compounds harder. Worth considering as a Wave 1.5 augment
   if HN/Reddit results disappoint.

5. **Two waves with Wave 1 quiet (recommended baseline alt).**
   Execute Wave 1 mechanics (README + site rebuild, distro
   packaging) but skip the HN/Reddit blitz; let Wave 1 grow via
   organic search + distro discovery only. Save *all* social
   attention for Wave 2. Trade-off: slower Wave 1 growth, but
   bigger Wave 2 explosion because no audience has been spent
   yet. Higher variance; probably better fit if confidence in the
   bridge phase delivery is high.

**Recommended: Two-wave loud, as written in Phases 1-6 + Bridge +
W2.1-W2.8.** Combines compounding attention events with credibility
preservation. Alternative #5 is the contingency if the bridge phase
takes longer than expected and Wave 1 attention has already been
spent.
