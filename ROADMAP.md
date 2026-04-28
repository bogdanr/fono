# Fono Roadmap

This is the public roadmap for [Fono](https://github.com/bogdanr/fono).
It is intentionally short and lives in the repo so it stays honest:
items move from **Planned** to **In progress** to **Shipped** as work
lands. Shipped items collect at the bottom with the date and the
release that carried them.

The [`CHANGELOG.md`](CHANGELOG.md) is the authoritative per-release
record; this file is the higher-altitude view of *where Fono is going*.

> **Maintainer note.** The release procedure (see `AGENTS.md`) updates
> this file: every roadmap item that ships in a release is moved into
> the **Shipped** section with the release tag and date *before*
> tagging.

---

## In progress

- **Wave 3 Slice B1 Thread C — cloud-mock equivalence lane.** Per-PR
  gate that exercises the cloud streaming path offline using committed
  WAV→JSON fixtures. Closes out Slice B1 and unlocks the `v0.3.0` tag.
  Plan: `plans/2026-04-28-wave-3-slice-b1-v1.md` Tasks C1–C9.

## Planned — next

- **Hardware- and language-aware local-model picker in the wizard.**
  Ask English-only vs multilingual first, filter the local Whisper
  shortlist by RAM/CPU/GPU, show per-language WER estimates inline,
  default the cursor to the largest model the machine can run
  comfortably. Plan:
  `plans/2026-04-28-wizard-local-model-selection-v1.md`.
- **Automatic translation pipeline.** STT → optional translate →
  cleanup → inject. Arbitrary BCP-47 `(source, target)` pairs,
  per-app overrides, batch + live parity, opt-in fast paths via
  Whisper `set_translate` and cloud `/audio/translations` when the
  target is English. Plan: `plans/2026-04-28-fono-auto-translation-v1.md`.
- **Self-update finishing pass.** ~85% landed; remaining items
  tracked under Wave 2 close-out. Plan:
  `plans/2026-04-27-fono-self-update-v1.md`.

## Planned — later

- **macOS port.** Native menubar (no tray crate dependency on macOS),
  CoreAudio capture, `CGEvent`-based injection, codesigned `.dmg`
  artefact in the release workflow.
- **Windows port.** Tray via `tray-icon`, WASAPI capture,
  `SendInput` injection, MSI / portable `.exe` artefact.
- **Wayland global hotkey via `org.freedesktop.portal.GlobalShortcuts`**
  once the portal is stable in mainstream compositors. Today Fono
  relies on compositor-side bindings to `fono toggle` on Wayland.
- **Streaming LLM cleanup.** Currently batch-only; the live overlay
  shows STT partials but cleanup runs once on commit. Streaming
  cleanup would let punctuation and casing settle as the user
  speaks.

## Won't do (for now)

- **Telemetry / phone-home.** Fono does not and will not collect
  usage data. See `docs/privacy.md`.
- **Llama-/Gemma-family default models.** Their licences are not
  OSI-approved. Available as opt-in only. See ADR
  `docs/decisions/0004-default-models.md`.
- **Web/Electron UI.** The whole point of Fono is to stay native and
  small. The tray + overlay + CLI are the UI surface.

---

## Shipped

Newest first. Each entry links to the release it shipped in.

- **In-memory cloud-STT language stickiness, peer-symmetric (no
  primary).** Self-heals one-off cloud STT misdetections (e.g. Groq
  Turbo flagging accented English as Russian) without breaking
  bilingual switchers. Wizard auto-adds English when only one
  non-English language is selected. — *Unreleased; queued for
  v0.3.0.*
- **Universal LLM cleanup clarification-reply fix.** Hardened
  default cleanup prompt, transcript fenced in `<<<` / `>>>`,
  refusal detector with raw-text fallback, `skip_if_words_lt`
  default raised to 3. Applies identically to all cloud and local
  cleanup backends. — *Unreleased; queued for v0.3.0.*
- **Streaming live-dictation pipeline reachable from the shipped
  binary.** `interactive` is now a default release feature; existing
  v0.2.1 users see live mode work for the first time after upgrade.
  — *v0.2.2, 2026-04-28.*
- **Self-update supply-chain hardening.** Per-asset `.sha256`
  sidecar verification; `--bin-dir` flag; refuses to overwrite
  package-managed paths. — *v0.2.2, 2026-04-28.*
- **`fono-bench` typed accuracy gate.** `ModelCapabilities`
  resolvers, split equivalence/accuracy thresholds, real-fixture CI
  bench gate using whisper `tiny.en` against committed baselines.
  — *v0.2.2, 2026-04-28.*
- **Streaming/interactive dictation mode.** Slice A foundation:
  streaming STT, latency budget, overlay live text, equivalence
  harness gating stream↔batch consistency per fixture. — *v0.2.1,
  2026-04-28.*
- **STT language allow-list.** `[general].languages: Vec<String>`
  replaces the single `language` scalar; constrained Whisper
  auto-detect with a hard ban on out-of-list languages. — *v0.2.1,
  2026-04-28.*
- **Overlay focus-theft fix on X11.** Override-redirect on the
  overlay window so it no longer intercepts `Shift+Insert`. — *v0.2.1,
  2026-04-28.*
- **Single-binary local stack (Whisper + Llama).** Both
  `whisper.cpp` and `llama.cpp` link into the same statically-built
  ELF; runtime SIMD probe; opt-in GPU acceleration via `accel-*`
  features. — *v0.2.0, 2026-04-27.*
- **Wizard local LLM path.** Tier-aware Qwen2.5 model selection
  (3B / 1.5B / 0.5B) alongside the existing local Whisper auto-download.
  — *v0.2.0, 2026-04-27.*
- **Hotkey defaults: F9 toggle / F8 push-to-talk.** Single keys, no
  default binding on any major desktop, easy to fire blind. — *v0.2.0,
  2026-04-27.*
- **First public release.** Audio → STT → LLM → inject pipeline wired
  end-to-end; local Whisper out of the box; multi-provider STT and LLM
  cleanup; tray with live provider switching; `fono record` /
  `transcribe` / `use` / `keys` / `doctor` / `hwprobe`. — *v0.1.0,
  2026-04-25.*

[v0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
[v0.2.0]: https://github.com/bogdanr/fono/releases/tag/v0.2.0
[v0.2.1]: https://github.com/bogdanr/fono/releases/tag/v0.2.1
[v0.2.2]: https://github.com/bogdanr/fono/releases/tag/v0.2.2
