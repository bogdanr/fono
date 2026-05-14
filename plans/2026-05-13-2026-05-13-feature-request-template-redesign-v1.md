# Feature Request Issue Template — Redesign

## Objective

Replace the current free-form `feature_request.md` with a template that is
(a) quick to fill, (b) visually clear when rendered on github.com, and
(c) more triagable for Fono maintainers — without overconstraining
drive-by suggestions. Cross-reference style with the existing
`bug_report.md` and `PULL_REQUEST_TEMPLATE.md`, and bake in the recurring
"new STT/LLM/TTS provider" shape that dominates Fono's feature backlog.

This is an **advisory plan only**. No files in `.github/` are modified by
this document.

---

## 1. Diagnosis of the current template

Source under review: `.github/ISSUE_TEMPLATE/feature_request.md:1-23`.

### What contributors typically skip

- **`## Alternatives considered`** (`.github/ISSUE_TEMPLATE/feature_request.md:17`)
  is a bare heading with no prompt comment and no example — most
  drive-by feature reports leave it empty, so it produces a dangling
  H2 in the rendered issue that adds noise without information.
- **`## Which design-plan task does this relate to?`**
  (`.github/ISSUE_TEMPLATE/feature_request.md:19-21`) presumes the
  reporter has read `docs/plans/2026-04-24-fono-design-v1.md` and knows
  task numbering. External contributors almost never do. The hint comment
  ("e.g. Task 4.3 (cloud STT backends)") is buried in an HTML comment so
  it's invisible in the rendered preview before someone starts editing.
- **`## Additional context`** (`.github/ISSUE_TEMPLATE/feature_request.md:23`)
  is the final section but has no body or comment — it commonly arrives
  empty and looks like an unfinished template.

### What maintainers always have to ask for afterwards

The current template captures **none** of the highest-signal triage axes
Fono maintainers care about:

1. **Category of request.** Looking at `docs/providers.md:9-29` and
   `CONTRIBUTING.md:69-85`, the single biggest class of feature asks is
   "add provider X" (STT/LLM/TTS). The template doesn't pre-route those,
   so every such issue requires a back-and-forth to identify which crate
   (`crates/fono-stt`, `crates/fono-llm`, or the upcoming TTS work) is
   affected.
2. **Platform parity.** Per `AGENTS.md` orientation, Fono targets Linux
   (X11 + Wayland on i3/sway/KDE/GNOME/Hyprland), Windows, and macOS.
   The `bug_report.md` template captures this for defects
   (`.github/ISSUE_TEMPLATE/bug_report.md:14-20`) but the feature
   template doesn't, leading to ambiguity over whether a request is
   cross-platform or Linux-only.
3. **Provider-specific metadata.** `CONTRIBUTING.md:74-82` enumerates
   exactly what a new provider PR must add (HTTP endpoint, API key
   env-var, supported models, streaming capability). Issues opened to
   propose those providers volunteer roughly none of this up-front. The
   hint sentence at `.github/ISSUE_TEMPLATE/feature_request.md:14-15`
   asks for "docs link + streaming + auth" in prose form, but free-form
   text in an HTML comment is consistently ignored.
4. **License compatibility.** A hard project rule per `AGENTS.md` is
   "no Llama/Gemma defaults — opt-in only" (referenced by
   `docs/decisions/0004-default-models.md`). Provider-add requests for
   non-OSI-licensed models routinely bypass this check at issue time
   and only get caught at PR/`deny.toml` review. Surfacing it in the
   template would shift the conversation left.
5. **Duplicate-search confirmation.** No "I searched existing issues
   and `ROADMAP.md`" checkbox exists, so duplicates of already-tracked
   roadmap items recur.

### How it renders today on GitHub

- Plain H2 headings with no visual rhythm; four near-identical section
  blocks. No emoji, no callouts, no `<details>` blocks, no checkboxes.
- The HTML-comment hints (lines 10, 14-15, 21) are invisible in the
  rendered preview that opens when a contributor clicks "Get started"
  — they only appear in the editor. This means the preview pane that
  GitHub uses for the picker description is just five empty H2s, which
  reads as "fill in five blank fields" rather than "answer five
  prompts."
- The `title:` prefix `"feat: <short summary>"` at line 4 is correct
  per Conventional Commits (`CONTRIBUTING.md:89-91`) but contributors
  often forget to delete the literal `<short summary>` placeholder,
  producing titles like `feat: <short summary> add Whisper.cpp option`.

---

## 2. Format recommendation — **pick: Option B (GitHub Issue Forms YAML)**

### Option A — restructured Markdown

Keep `.md`, add emoji section headers, blockquote callouts, a
`<details>` block for provider-specific questions, and a "before
submitting" checklist mirroring `PULL_REQUEST_TEMPLATE.md:5-12`.

Pros: zero new syntax, contributors can freely edit any field after
submission, renders fine on mobile, no required-field friction.

Cons: still free-form — no enforced category, no platform checkboxes
the contributor *must* click, hint comments remain invisible in the
picker preview, and triage labels can't be auto-attached on submit.

### Option B — GitHub Issue Forms (`.yml`)

Convert to the YAML schema (`type: dropdown`, `type: checkboxes`,
`type: textarea` with `placeholder` + `value`, `validations.required`).

Pros:

- Placeholders and descriptions render **in the form itself**, not just
  in the editor — solves the invisible-hints problem outright.
- A `dropdown` for category gives maintainers a free, structured triage
  signal that drives label automation
  (`labels: ["enhancement", "stt"]` etc. via the top-level `labels:`
  key plus follow-on Actions if desired).
- `checkboxes` for platforms is one click, encouraging contributors to
  actually answer the cross-platform question.
- `required: true` on the smallest possible set of fields enforces
  minimum triage quality without driving away drive-by reporters
  (see §5 below for the required/optional split).
- Mobile rendering of Issue Forms on github.com is now first-class.

Cons:

- **Cannot edit form fields inline after submission** — answers become
  body text and can only be edited as raw Markdown afterwards. For
  Fono's audience (developers comfortable editing Markdown), this is a
  minor inconvenience, not a blocker.
- YAML schema has a small learning curve for maintainers when iterating
  on the template, but Fono already maintains workflow YAML in
  `.github/workflows/`, so the team is comfortable with the format.
- Subtly different "Get started" UX may surprise repeat reporters.

### Comparison summary

| Axis | Markdown (A) | Issue Forms (B) |
|------|--------------|-----------------|
| Time-to-fill | ~Same | **Faster** (clickable inputs, no Markdown to type) |
| Render quality | Decent with effort | **Best** (native form widgets) |
| Triage signal quality | Free-form prose | **Structured** (category dropdown, platform checkboxes) |
| Maintenance burden | Low | Low-to-moderate (YAML schema) |
| Discoverability of conventions (design plan link, license rule) | Hidden in HTML comments | **Visible** as field `description` + prefilled `value` |
| Post-submit editability | Inline | Body-only |

**Decision: Option B (Issue Forms YAML).** The triage-signal and
visibility wins are large; the post-submit-edit limitation is acceptable
for a Rust/Linux developer audience.

---

## 3. Proposed template (verbatim, ready to paste)

Filename: `.github/ISSUE_TEMPLATE/feature_request.yml`
(replaces `.github/ISSUE_TEMPLATE/feature_request.md`).

```yaml
name: Feature request
description: Propose a new capability, provider backend, or UX improvement.
title: "feat: <short summary>"
labels: ["enhancement"]
body:
  - type: markdown
    attributes:
      value: |
        Thanks for helping shape Fono. Two quick asks before you start:
        please skim [ROADMAP.md](../blob/main/ROADMAP.md) and the open
        `enhancement` issues to avoid duplicates. Most fields below are
        optional — answer what you can.

  - type: checkboxes
    id: prechecks
    attributes:
      label: Pre-flight
      options:
        - label: I searched existing issues and ROADMAP.md and did not find a duplicate.
          required: true

  - type: textarea
    id: problem
    attributes:
      label: Problem
      description: One or two sentences. What user-facing need is not served today?
      placeholder: "Dictating into IntelliJ on Wayland inserts nothing because xdotool isn't available."
    validations:
      required: true

  - type: dropdown
    id: category
    attributes:
      label: Category
      description: What kind of change is this? Biggest triage signal — please pick one.
      options:
        - New STT provider
        - New LLM provider
        - New TTS provider
        - Desktop UX (tray, overlay, hotkey, injection)
        - Packaging / distribution
        - Documentation
        - Other
    validations:
      required: true

  - type: checkboxes
    id: platforms
    attributes:
      label: Affected platforms
      options:
        - label: Linux (X11)
        - label: Linux (Wayland)
        - label: macOS
        - label: Windows

  - type: textarea
    id: proposal
    attributes:
      label: Proposed solution
      description: Sketch the feature. Rough is fine.
      placeholder: "Add a `wtype` fallback path in fono-inject when running under Wayland and enigo fails."

  - type: textarea
    id: provider-details
    attributes:
      label: Provider details (only if Category is a new STT / LLM / TTS provider)
      description: |
        Fill these in if you're proposing a provider backend. Skip otherwise.
        See `CONTRIBUTING.md` "Adding a new STT or LLM provider backend".
      value: |
        - Upstream docs URL:
        - Streaming support (yes / no / pseudo):
        - Auth model (Bearer / API key header / OAuth / none):
        - Default model name:
        - License of the model weights (OSI-approved? Llama/Gemma-family models are opt-in only — see `docs/decisions/0004-default-models.md`):

  - type: input
    id: design-plan
    attributes:
      label: Design-plan tie-in (optional)
      description: Phase or task in the Fono design plan, if you know it.
      value: "docs/plans/2026-04-24-fono-design-v1.md — phase/task unknown"

  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives considered
      description: Optional — workarounds you've tried or other designs you weighed.

  - type: textarea
    id: context
    attributes:
      label: Additional context
      description: Optional — links, screenshots, prior art.
```

Line count: ~75 YAML lines (within the "tight Issue Forms" budget;
exactly 6 fields are marked `required: true` indirectly — actually 2
required validations: the duplicate-check box and the problem
statement; everything else is optional or has a prefilled `value`).

---

## 4. Visual polish recommendations

### Emoji policy — tension flagged explicitly

`AGENTS.md` / the repo's `non_negotiable_rules` say "no emojis unless
the user explicitly requests." The user **has** requested visual
improvement for this file, which puts emoji on the table for the
template **only**. My recommendation is to **still omit emoji** in the
final YAML, because:

- The Forms UI already provides visual rhythm (form widgets, native
  required-field markers, section labels in a sans-serif sidebar).
  Emoji on top adds clutter, especially in dark theme where the
  default emoji set has poor contrast against the editor chrome.
- `bug_report.md` and `PULL_REQUEST_TEMPLATE.md` use zero emoji. Adding
  them only to the feature template breaks consistency.
- Forms render identically in dark/light themes; emoji can render
  inconsistently across OS emoji fonts.

If, after preview, the maintainers feel the form is still visually flat,
a single leading glyph per top-level section (📝 / 🎯 / 🧩 / 🖥️ / 🔌)
is the cheapest reversible upgrade — done in the `label:` field of each
block. Document that decision in the PR that lands the template so the
"no emoji" rule isn't violated silently.

### Other polish

- **Heading hierarchy:** Forms don't have explicit headings — field
  `label` is rendered as the section title, and `description` as
  smaller helper text. Keep labels short (2-4 words); push elaboration
  into `description`. The `- type: markdown` intro block uses one
  short paragraph; avoid additional H1/H2 inside it.
- **`<kbd>` / `<details>` / blockquote callouts:** Issue Forms render
  Markdown inside `description` and `value` fields. Use a single
  blockquote callout in the intro markdown block for the duplicate-check
  reminder. Do **not** use `<details>` inside Forms — collapsed sections
  inside a textarea are a known rendering gotcha. The Markdown `_md`
  fallback (Option A) would have used `<details>` for the provider
  sub-section; Forms achieves the same "tuck advanced fields away"
  outcome through a single conditional-by-instruction textarea
  ("only if Category is …").
- **Line length:** keep YAML string values ≤ 100 cols; descriptions
  wrap naturally in the rendered form. The placeholder examples should
  be ≤ 80 cols to avoid horizontal scroll on mobile.
- **Dark vs light theme:** Forms inherit the user's GitHub theme.
  Plain text + native widgets render identically in both. No special
  handling needed.
- **Title prefix `feat:`** keeps Conventional Commits alignment with
  `CONTRIBUTING.md:89-91`. Keep the `<short summary>` placeholder —
  Forms-mode shows it inline, less prone to "left in by accident" than
  the Markdown variant.

---

## 5. Risk / trade-off analysis

1. **Forms can't be edited inline after submission.**
   For Fono's audience (Rust devs comfortable with Markdown), this is
   minor — they can edit the rendered body afterwards just like any
   issue. Document this in the maintainer-side `docs/` only if
   complaints surface; do not preemptively warn end users in the form.

2. **Required-field friction vs drive-by suggestions.**
   Recommended required set (minimum viable triage signal):
   - Pre-flight checkbox (duplicate search confirmation) — required.
   - Problem statement (`textarea`) — required.
   - Category (`dropdown`) — required.

   Everything else is optional, including platforms (a single-platform
   drive-by shouldn't be forced to click anything), the proposal
   textarea, provider details, and the design-plan tie-in. Three
   required fields is the project's sweet spot: meaningful enough to
   prevent empty issues, light enough that a one-liner suggestion
   ("add Cartesia STT") fits in under 30 seconds.

3. **Maintaining two formats is bad — pick one.**
   Migrate fully to `.yml`. **Delete** `feature_request.md` in the same
   PR. Do not ship a hybrid. GitHub's picker happily mixes `.md` and
   `.yml` files, which would tempt drift.

4. **Compatibility with `bug_report.md`.**
   Two options:
   - **Staged** (recommended): land the feature-request migration
     first, observe issue quality for one or two release cycles, then
     migrate `bug_report.md` to `bug_report.yml` reusing the same
     dropdowns/checkboxes vocabulary (platform, audio backend, STT
     backend, etc., already encoded as bullets at
     `.github/ISSUE_TEMPLATE/bug_report.md:14-20` and trivially
     convertible). Staging keeps the blast radius small.
   - **Together**: cheaper review overhead but doubles risk if
     anything renders unexpectedly.

   Pick staged. Also consider adding `.github/ISSUE_TEMPLATE/config.yml`
   in the same PR (see §7) to lock the picker order — see Alternative 1.

---

## 6. Verification criteria

- [ ] Opening **New issue** on github.com shows the redesigned card
      with the updated description text.
- [ ] Clicking the card opens a form (not a raw Markdown editor),
      with all field labels and placeholders visible without scrolling
      on a 1080p viewport.
- [ ] All three required fields block submission with the standard
      "This field is required" inline error when empty.
- [ ] Submitting with only the required fields produces a clean issue
      body where unfilled sections are omitted entirely (Forms default
      behaviour) — verify no stray empty H2s remain.
- [ ] Auto-applied labels (`enhancement` from the top-level `labels:`
      key) appear on the created issue. If category-specific labels
      (`stt`, `llm`, `tts`, `packaging`, `docs`) are desired, that
      requires a small Action workflow — out of scope here, but record
      as a follow-up.
- [ ] Mobile rendering: open the same form on a phone-width viewport
      (Chrome DevTools 375 px) and confirm dropdown + checkboxes are
      tappable and labels don't truncate.
- [ ] Dark and light theme: switch GitHub theme and confirm contrast
      on the description helper text.
- [ ] A test issue filed by a maintainer reproduces the structure
      expected; delete the test issue after verification.

---

## 7. Alternative directions (≥ 2)

1. **Add `.github/ISSUE_TEMPLATE/config.yml` first** to lock the picker
   order and add external `contact_links` (e.g. to Discussions for
   "is this a bug or a feature?" pre-triage, or to a Matrix/IRC
   channel if Fono has one). This is orthogonal to the form/markdown
   choice and can ship in the same PR as the feature-request migration
   at near-zero cost.

2. **Split `feature_request` into two templates:**
   - `feature_request_provider.yml` — focused on STT/LLM/TTS additions
     with all the provider-specific fields required.
   - `feature_request_general.yml` — a minimal three-field form for
     everything else.

   Rationale: provider-add issues are structurally different from "add
   a tray submenu" issues and the unified template forces one to
   carry the other's vocabulary. Trade-off: more picker entries
   (mitigated by `config.yml` ordering and crisp descriptions), but
   each individual form is shorter and more focused. Worth doing if
   provider-add volume exceeds ~30% of feature issues post-launch.

3. **Keep Markdown (Option A) but invest in a richer presentation.**
   Recommend only if the maintainers want to preserve post-submit
   inline editability or have observed users struggling with Issue
   Forms elsewhere. Concretely: emoji section headers, a leading
   blockquote callout for the duplicate-check, a
   `<details><summary>Provider request? Open this</summary>…</details>`
   block, and a `## Before submitting` checklist mirroring
   `PULL_REQUEST_TEMPLATE.md:5-12`. Lower ceiling on triage quality
   than Option B but zero schema risk.

---

## Citations (current-file references used in this plan)

- `.github/ISSUE_TEMPLATE/feature_request.md:1-23`
- `.github/ISSUE_TEMPLATE/bug_report.md:14-20`
- `.github/ISSUE_TEMPLATE/bug_report.md:1-39`
- `.github/PULL_REQUEST_TEMPLATE.md:5-12`
- `CONTRIBUTING.md:69-85`
- `CONTRIBUTING.md:89-91`
- `docs/providers.md:9-29`
- `docs/providers.md:85-94`
- `docs/plans/2026-04-24-fono-design-v1.md:1-70`
- `AGENTS.md` (project orientation — Llama/Gemma rule, cross-platform
  targets)
