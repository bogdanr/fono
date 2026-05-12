# Fono — Public Launch Strategy

## Objective

Position Fono as **the** voice-dictation tool for Linux first, then the
cross-platform privacy-first alternative to Whispr Flow / Superwhisper /
MacWhisper. Reach top-3 mindshare in the "voice → text → editor" category
within 6 months of launch.

Target metrics (12 months):
- 10,000+ GitHub stars (currently low; v0.6.1 just shipped).
- HN front page once during Wave 1, once during Wave 2.
- 1,000+ daily active users (best-effort estimate via update-check pings if
  we ever add opt-in usage stats; otherwise via release-asset download counts
  which we already have for free).
- Top result on Google for `linux voice dictation`, `whisper dictation linux`,
  `wayland voice typing`.
- Packaged in AUR, Homebrew (macOS Wave 2), Flathub, Nixpkgs official channel,
  Debian unstable.

## Strategic Posture

**Two-wave launch.** The product is already mature enough on Linux to win
the Linux-native niche outright today; waiting until macOS/Windows ship
delays that capture by months and lets competitors define the category.
Wave 1 captures Linux + the privacy-conscious cross-platform user base that
will tolerate "Linux-only for now". Wave 2 converts the broader desktop
market once macOS or Windows lands.

**Do NOT** wait for full feature parity with the roadmap before any public
push — that's a year of silent development with zero compounding.

## Implementation Plan

### Phase 1 — Pre-launch hardening (Wave 1, week 1)

- [ ] **Task 1.1 — Pin a v0.7.0 "Public Beta" release.** Cut from current
  main once Phase 1 ships. Bump `Cargo.toml`, `CHANGELOG.md`, `ROADMAP.md`
  per the AGENTS.md hard rules. Tag includes "Public Beta" wording in the
  GitHub release body to set expectations on Linux-only scope.
- [ ] **Task 1.2 — Add a 30-second screencast/GIF to the top of `README.md`.**
  Show: hotkey press → speak → text appears in editor + browser + terminal.
  Record at 1080p; export as both MP4 (for fono.page) and optimised GIF
  (≤8 MB, for README rendering on github.com). Without this, every HN /
  Reddit submission converts at 1/5 the rate.
- [ ] **Task 1.3 — Add a head-to-head comparison table to `README.md`** under
  a `## Why Fono` section. Columns: Fono, Whispr Flow, Superwhisper,
  MacWhisper, VoiceInk, Talon, Dragon. Rows: Linux native, Wayland, GPL,
  no telemetry, single binary, local Whisper, local LLM cleanup, cloud
  providers, hot-swap, wake-word, Wyoming/HA, price. Be ruthlessly factual;
  a single asterisked claim destroys credibility on HN.
- [ ] **Task 1.4 — Audit the live state of `fono.page`.** Sub-tasks: hero
  section with the screencast embedded, install one-liner above the fold,
  feature grid mirroring the README comparison, "Status: Public Beta — Linux
  only" banner with macOS/Windows ETA, link to GitHub Releases. Drop
  marketing-speak; lead with the screencast + the install one-liner.
- [ ] **Task 1.5 — Submit to AUR, Flathub, Homebrew (linux), Nixpkgs.** AUR
  PKGBUILD already exists; verify it tracks `v0.7.0`. Flathub manifest is
  net-new (the static binary makes the manifest trivial: copy single ELF +
  `.desktop` + icon). Homebrew Linux formula adds discoverability. Nixpkgs
  upstream PR converts the existing flake into a channel-shipped derivation.
- [ ] **Task 1.6 — Recovery messaging audit.** Walk every user-visible error
  path with fresh eyes (first-run wizard, install failures, hotkey conflict,
  empty transcript, cloud 429, missing API key). Each should name the
  problem in plain English and tell the user the *one command* that fixes
  it. This is what differentiates a polished release from a beta.
- [ ] **Task 1.7 — Update-channel telemetry confirmation.** `fono update`
  already pings GitHub releases. Confirm the existing path is privacy-clean
  (no UA fingerprinting, no per-host ID) and document it explicitly in
  `docs/privacy.md`. This is the *one* place we get download-count signal,
  for free, so make sure it's bulletproof.

### Phase 2 — Wave 1 launch (week 2-3)

- [ ] **Task 2.1 — Stagger announcements over 4-7 days.** Day 1: r/linux,
  r/commandline, r/voicedictation, r/i3wm, r/swaywm, r/KDE, r/gnome
  (different angles per sub: "single binary", "Wayland-first", "tiling-WM
  friendly", "KDE Plasma integration"). Day 2: HN ("Show HN: Fono — a
  20 MB Linux voice dictation daemon that runs Whisper locally"). Day 3:
  Phoronix tip submission. Day 4: lobste.rs. Day 5-7: Twitter/Mastodon
  threads breaking down individual differentiators (Wyoming, Vulkan
  auto-switch, multi-language self-correction).
- [ ] **Task 2.2 — Engage every comment in the first 24 hours after HN
  submission.** This is the single highest-leverage activity of the entire
  launch. HN front-page survival depends on author responsiveness.
  Template responses prepared in advance for: "why not Python?", "why GPL
  not MIT?", "is it really faster than Whispr Flow?", "Wayland support?",
  "macOS when?".
- [ ] **Task 2.3 — Reach out to 3-5 Linux YouTubers** (TheLinuxExperiment,
  Brodie Robertson, DistroTube, Mental Outlaw) with personalised emails
  including the screencast. Free product, zero-string demos. Their audiences
  are exactly Fono's wedge.
- [ ] **Task 2.4 — Pin a `Discussions` post on GitHub** titled "Public Beta —
  what to install, what works, what's coming". Acts as the single FAQ link
  for every comment thread elsewhere.

### Phase 3 — Wave 1 momentum (months 1-3)

- [ ] **Task 3.1 — Wake-word activation (`ROADMAP.md` "Up next").** Big
  diff vs every keyboard-driven competitor. openWakeWord integration; runs
  on a fraction of one CPU core; idle-wake → dictate → idle. Ship as v0.8.0.
  This is a tweetable / blogpost-ready feature in itself.
- [ ] **Task 3.2 — REST API + MCP server (`ROADMAP.md` "On the horizon").**
  Capture the AI-coder wave: Cursor / Claude Code / Aider users want to
  speak prompts. Fono-as-MCP-server is the cleanest delivery. The
  Unix-socket IPC already exposes every primitive; HTTP+MCP wrappers are
  ~1000 LOC. Ship as v0.9.0. Cross-promote in r/cursor, r/ChatGPTCoding,
  Anthropic Discord, MCP server registries.
- [ ] **Task 3.3 — Translation pipeline (`ROADMAP.md` "Up next").** Speak
  Romanian, type English. Speak any → type any. The free fast-path on
  Whisper translate-mode + cloud `audio/translations` is straightforward;
  the per-app rules and live parity are the real work. Massive
  international-user appeal (currently every dictation tool is English-
  centric). Ship as v0.10.0.
- [ ] **Task 3.4 — Weekly progress thread on /r/fono (create the sub) +
  Mastodon**. Doesn't have to be huge; consistency compounds. Each thread
  links the latest released CHANGELOG section.
- [ ] **Task 3.5 — Aggressive packaging push.** Pursue Debian (unstable
  → testing path), Fedora COPR then proper Fedora package, openSUSE
  Tumbleweed, Void Linux. Each new repo unlocks a discovery channel.

### Phase 4 — Wave 2 prep (months 3-5)

- [ ] **Task 4.1 — Decide: macOS first or Windows first?** Recommendation:
  **macOS first.** (a) macOS users overlap heavily with the privacy-
  conscious Linux audience already paying $15/mo for Superwhisper; this is
  Fono's strongest cross-platform conversion target. (b) Windows users have
  free WhisperWriter / Buzz / Vibe alternatives; the differentiation gap is
  smaller. (c) macOS Apple Silicon CoreML acceleration is technically
  interesting and Whisper.cpp already supports it cleanly. Document the
  decision in a new ADR.
- [ ] **Task 4.2 — Port the platform-specific layers to macOS.** Audit
  per-crate: `fono-audio` (cpal already supports CoreAudio), `fono-hotkey`
  (`global-hotkey` supports macOS), `fono-inject` (CGEventPost via
  `enigo` macOS backend — already wired), `fono-tray` (need a non-SNI tray
  backend; `tray-icon` had macOS support — re-add behind a feature flag,
  or write a thin AppKit wrapper). Overlay: winit already supports macOS.
  This is the riskiest task; budget 4-6 weeks of full-time focused work.
- [ ] **Task 4.3 — Code-sign + notarise the macOS build.** Apple Developer
  Program ($99/yr) is non-negotiable; an unsigned binary won't survive
  Gatekeeper for non-technical users. Set up a `.dmg` with a drag-to-
  Applications layout. Ship via the existing GitHub releases workflow plus
  a Homebrew Cask formula.
- [ ] **Task 4.4 — Add a curated 12-fixture macOS WER baseline** to the
  release-time cloud-equivalence gate so we catch macOS-specific
  regressions.

### Phase 5 — Wave 2 launch (month 5-6)

- [ ] **Task 5.1 — v1.0 release.** Drop the "Public Beta" label. CHANGELOG
  entry summarises every Wave 1 milestone. ROADMAP "Shipped" section
  reorganised. Tag is the public-stability commitment.
- [ ] **Task 5.2 — Big push: Product Hunt + HN + Twitter influencer
  outreach + YouTube video collaboration.** Product Hunt timing must be
  Tuesday-Thursday morning Pacific. HN submission with title "Show HN:
  Fono 1.0 — voice dictation for Linux and macOS, single 20 MB binary".
  Pre-line up 5-10 supporters to upvote in the first 30 min (against HN
  rules to ask for upvotes; ask only for honest feedback comments,
  upvotes follow naturally).
- [ ] **Task 5.3 — Paid: $200-500 sponsored Phoronix banner for 1 week.**
  Phoronix readership is exactly Fono's audience. The CTR will pay back
  the spend in stars + downloads.
- [ ] **Task 5.4 — Submit a CFP for FOSDEM 2027 Open Source AI Devroom.**
  In-person presence at the largest open-source conference is permanent
  capital; even if rejected the talk material becomes a YouTube video.
- [ ] **Task 5.5 — Post-launch retro.** What converted? What didn't? Adjust
  Phase 6 priorities accordingly.

### Phase 6 — Sustained growth (month 6-12)

- [ ] **Task 6.1 — Windows port.** Same playbook as macOS; cleaner because
  no notarisation, but uglier because installer culture is ad-hoc. Ship
  as `.msi` via WiX or `.exe` via Inno Setup; chocolatey + winget formulae
  for power users.
- [ ] **Task 6.2 — Hover-context injection (`ROADMAP.md` experimental).**
  Differentiator nobody else has. Tweetable demos. Ships as v1.1.
- [ ] **Task 6.3 — Plugin / extension surface.** Editor extensions
  (VS Code, Neovim, Helix) that expose Fono commands inline. Each plugin
  is its own discovery channel.
- [ ] **Task 6.4 — Sustainability.** GitHub Sponsors page; "Pro" tier
  with priority support but identical OSS binary; consider a hosted
  enterprise Wyoming server offering for HA / industrial use cases.
  *Never* introduce a paid tier of the binary itself — it would
  immediately fork.

## Verification Criteria

- Wave 1 measurable success:
  - HN front page (top 30) reached at least once.
  - 1,000+ GitHub stars within 30 days of v0.7.0 tag.
  - 5+ packaging channels live (AUR, Flathub, Nixpkgs, Homebrew Linux,
    Debian unstable).
  - At least 1 Linux-influencer YouTube video published.
- Wave 2 measurable success:
  - Product Hunt #1 of the day on launch day.
  - HN front page (top 10) reached on launch day.
  - 5,000+ GitHub stars within 30 days of v1.0 tag.
  - macOS download share ≥ 30% of total in month 6.
- Sustained growth:
  - 10,000+ stars by month 12.
  - Average time-to-first-response on issues < 48h.
  - Release cadence ≥ one user-facing feature per month.

## Potential Risks and Mitigations

1. **Whispr Flow / Superwhisper ship a free Linux build during Wave 1.**
   Mitigation: Fono's GPL + single-binary + Wyoming + multi-language
   self-correction stack is hard to copy in a quarter; lean into those
   in messaging. Don't compete on parity — compete on principle (privacy,
   FOSS, no subscription).

2. **macOS port lands buggy and tarnishes the Linux reputation.**
   Mitigation: ship macOS as a separate "Public Beta" tag for 1-2 minor
   releases before promoting to v1.0. Apple-specific bug reports go to a
   dedicated triage label so Linux users see a clean issue tracker.

3. **HN/Reddit response is muted (the "Show HN tax" — submitted on a slow
   day, buried by competing posts).** Mitigation: stagger announcements
   over a week (Phase 2 Task 2.1) and re-submit to HN with a substantive
   feature-add update at v0.8.0 release; HN's `noprocrast` resubmission
   policy permits one substantive resubmission per significant change.

4. **Homebrew / Flathub / Nixpkgs review delays block packaging push.**
   Mitigation: open the upstream PRs immediately at Wave 1 (Phase 1
   Task 1.5) so the review queue starts ticking before the launch hype
   window opens.

5. **Vendor-specific GPU regressions (NVIDIA driver bugs, AMD ROCm
   weirdness) surface in mass deployment that the bench harness misses.**
   Mitigation: the v0.5.0 CPU/GPU auto-switch already gracefully falls
   back; expand the bench-harness baseline to include 2-3 contributed
   GPU configurations via a community-run nightly bench script.

6. **A single GPL-incompatible code dependency creep gets caught
   post-launch.** Mitigation: `deny.toml` is already enforcing license
   compatibility; add a pre-tag CI gate that produces a license report
   and fails the release if any dependency lacks an OSI-approved
   GPL-compatible license. (Already partly enforced; tighten before
   v0.7.0.)

7. **Burnout from sustained launch + maintenance + macOS port load.**
   Mitigation: explicitly time-box Phase 4 (macOS) at 6 weeks; if it
   slips, ship Wave 1.5 with whatever's done and slip Wave 2 by a
   month rather than working evenings. Sustained pace > heroic sprints.

## Alternative Approaches

1. **Single big-bang v1.0 launch, no Wave 1.** Wait until macOS lands,
   then a single Product Hunt + HN push. Pro: only one shot at hype, all
   forces concentrated. Con: 4-6 months of silent dev with zero
   compounding star growth, zero feedback, zero packaging-channel work
   in parallel; HN audiences are forgiving of "v0.x Linux beta" in a way
   they're not of "v1.0 with bugs". **Rejected.**

2. **Linux-only forever, no macOS/Windows port.** Pro: smaller scope,
   tighter focus. Con: caps total addressable audience at single-digit
   percentage of desktop, locks Fono out of the conversation everywhere
   except Linux subreddits. The privacy-conscious macOS user willing to
   pay $15/mo for Superwhisper is exactly Fono's ideal user — leaving
   them on the table is a strategic mistake. **Rejected.**

3. **Pivot to a paid tier for the binary itself ("Fono Pro" with cloud
   providers preconfigured).** Pro: revenue. Con: GPL means anyone can
   strip the gate and republish; the FOSS narrative collapses; we lose
   the principle-based differentiator that makes the product
   interesting. **Rejected — see Phase 6 Task 6.4 for the sustainable
   funding model.**

4. **Skip Phase 1 hardening and launch raw.** Pro: ship today. Con: every
   missing screencast / packaging channel / comparison table is an order
   of magnitude in conversion — first impressions on HN don't repeat.
   **Rejected — Phase 1 is 1-2 weeks of work for a 5-10x conversion lift.**
