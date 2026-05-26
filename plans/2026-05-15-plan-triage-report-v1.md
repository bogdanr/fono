# Plan Triage Report — 2026-05-15

Audit of every file in `/mnt/nvme0n1p5/Work/fono/plans/` (excluding
`plans/closed/`) cross-referenced against `CHANGELOG.md`, `ROADMAP.md`,
`docs/status.md`, and the shipped feature surface in `crates/`.
71 active plan files inspected.

Legend for **classification**:

- `CLOSE-Completed` — every verification criterion is met by shipped code/docs.
- `CLOSE-Superseded-by-vN` — older revision of a plan whose latest revision is the keeper (or is itself shipped).
- `CLOSE-Superseded-by-OTHER` — superseded by a differently-named plan.
- `CLOSE-Abandoned` — goal no longer relevant.
- `MERGE-INTO-X` — overlaps substantially with another plan.
- `KEEP-Active` — still represents pending work.
- `UNCLEAR` — needs human decision.

## 1. Per-plan classification

| Filename | Classification | Reason | Action |
|---|---|---|---|
| `2026-04-27-fono-interactive-v1.md` | CLOSE-Superseded-by-v6 | v1; the v3/v4/v5 plans explicitly carry `Status: Proposed (supersedes …)` headers leading up to v6, and v6's Slice A shipped per `docs/status.md:1794-1869` and CHANGELOG v0.2.1. | `git mv` to `closed/` with `Status: Superseded` |
| `2026-04-27-fono-interactive-v2.md` | CLOSE-Superseded-by-v6 | Same chain. | Same |
| `2026-04-27-fono-interactive-v3.md` | CLOSE-Superseded-by-v6 | Header already says `supersedes v2`; v6 is the keeper. | Same |
| `2026-04-27-fono-interactive-v4.md` | CLOSE-Superseded-by-v6 | Header says `supersedes v3`. | Same |
| `2026-04-27-fono-interactive-v5.md` | CLOSE-Superseded-by-v6 | Header says `supersedes v4`. | Same |
| `2026-04-27-fono-interactive-v6.md` | CLOSE-Completed | Slice A landed in five DCO-signed commits (`7fbf974…`); shipped in v0.2.1; Slice B/C carried forward in `wave-3-slice-b1-*` plans (also shipped) and roadmap-tier follow-ups already moved to closed. | `git mv` with `Status: Completed` |
| `2026-04-27-fono-self-update-v1.md` | CLOSE-Completed | The plan's own `## Status` header records "~85% landed in `3e2c742`"; the remaining work (sidecar verification, `--bin-dir`) was completed in v0.2.2 Wave 2 Thread B (`CHANGELOG.md:1478-1493`). | `git mv` with `Status: Completed` |
| `2026-04-27-local-stt-llm-resolution-v1.md` | CLOSE-Completed | The `--allow-multiple-definition` link trick that resolves the ggml symbol collision shipped in v0.2.0 (`CHANGELOG.md:1630-1633`, ADR 0018). | `git mv` with `Status: Completed` |
| `2026-04-28-2026-04-28-wizard-local-model-selection-v1.md` | CLOSE-Completed | Shipped as the v0.3.5 wizard refresh (`CHANGELOG.md:262-267`: "Smarter first-run setup … hardware-aware shortlist capped at three"). | `git mv` with `Status: Completed` |
| `2026-04-28-doc-reconciliation-v1.md` | CLOSE-Completed | Executed in `status.md:1587-1649`; all referenced ADR backfills, plan ticks, and three plan moves to `closed/` are visible in the tree today. | `git mv` with `Status: Completed` |
| `2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md` | CLOSE-Completed | Closed by Wave 2 Thread A (`status.md:1535-1546`, CHANGELOG v0.2.2 "typed `ModelCapabilities` API + split thresholds"). | `git mv` with `Status: Completed` |
| `2026-04-28-fono-auto-translation-v1.md` | KEEP-Active | Listed as "Up next" in `ROADMAP.md:25-42`. Not shipped. | Leave in place |
| `2026-04-28-linux-notification-unification-via-notify-send-v1.md` | CLOSE-Completed | Shipped in v0.3.5 (`CHANGELOG.md:1223-1235`: "Linux desktop notifications now route through `notify-send`"). | `git mv` with `Status: Completed` |
| `2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md` | CLOSE-Completed | Shipped in v0.3.0 (`CHANGELOG.md:1395-1411`) and `docs/status.md:1365-1427`. | `git mv` with `Status: Completed` |
| `2026-04-28-multi-language-stt-no-primary-v1.md` | CLOSE-Superseded-by-v3 | v3 is the executed iteration; `status.md:1276-1283` documents rejection rationale for v1/v2. | `git mv` with `Status: Superseded` |
| `2026-04-28-multi-language-stt-no-primary-v2.md` | CLOSE-Superseded-by-v3 | Same chain. | Same |
| `2026-04-28-multi-language-stt-no-primary-v3.md` | CLOSE-Completed | Shipped in v0.3.0 (`CHANGELOG.md:1422-1456` plus ADR 0017). | `git mv` with `Status: Completed` |
| `2026-04-28-stt-hallucination-strip-and-llm-diff-log-v1.md` | CLOSE-Superseded-by-v3 | Three-layer fix shipped in v0.3.5 via v3 (`CHANGELOG.md:1161-1184`). | `git mv` with `Status: Superseded` |
| `2026-04-28-stt-hallucination-strip-and-llm-diff-log-v2.md` | CLOSE-Superseded-by-v3 | Same. | Same |
| `2026-04-28-stt-hallucination-strip-and-llm-diff-log-v3.md` | CLOSE-Completed | Shipped v0.3.5. | `git mv` with `Status: Completed` |
| `2026-04-28-stt-language-allow-list-v1.md` | CLOSE-Completed | Shipped in v0.2.1 (`CHANGELOG.md:1586-1607`). | `git mv` with `Status: Completed` |
| `2026-04-28-streaming-feedback-and-truncation-fixes-v1.md` | CLOSE-Completed | All three symptoms shipped in v0.3.4/v0.3.5 (`CHANGELOG.md:1237-1286`: hotkey-INFO log, `hold_release_grace_ms`, 429 desktop notification). | `git mv` with `Status: Completed` |
| `2026-04-28-streaming-rate-limit-controls-v1.md` | CLOSE-Completed | `interactive.streaming_interval` shipped v0.3.3 (`CHANGELOG.md:1290-1302`); 429 desktop notification + 60 s throttle shipped v0.3.5. | `git mv` with `Status: Completed` |
| `2026-04-28-wave-2-close-out-v1.md` | CLOSE-Completed | Three threads documented as landed in `status.md:1527-1586`; CHANGELOG v0.2.2. | `git mv` with `Status: Completed` |
| `2026-04-28-wave-3-slice-b1-v1.md` | CLOSE-Completed | Threads A+B shipped (`status.md:1429-1502`); Thread C migrated to the v2 plan (`thread-c-live-groq-v2`) which itself shipped. | `git mv` with `Status: Completed` |
| `2026-04-28-wave-3-slice-b1-thread-c-live-groq-v2.md` | CLOSE-Completed | Live Groq equivalence gate landed in v0.3.0 (`CHANGELOG.md:1370-1392`; `status.md:1209-1263`). | `git mv` with `Status: Completed` |
| `2026-04-29-2026-04-29-client-server-wyoming-and-native-v1.md` | CLOSE-Superseded-by-v2 | v2 (`-fono-and-mdns-v2`) is the executed plan; both slices 1–4 shipped across v0.3.7 and v0.4.0. | `git mv` with `Status: Superseded` |
| `2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md` | CLOSE-Completed | Codec, Wyoming server, Wyoming client, mDNS discovery, tray submenu all in v0.3.7 + v0.4.0 (`CHANGELOG.md:1006-1085`, `:882-922`). Native-Fono WebSocket Slice 5–6 not landed but per CHANGELOG/ROADMAP they were descoped; the v2 plan's verification criteria for the Wyoming-side work are met. Confirm with maintainer if WebSocket slices should re-open. | `git mv` with `Status: Completed` (see Open Questions §4) |
| `2026-04-29-alsa-plugin-filter-and-cache-v1.md` | CLOSE-Superseded-by-OTHER | Superseded by `2026-04-29-pulseaudio-first-microphone-enumeration-v1.md` which made the cpal/ALSA enumeration path unreachable on PulseAudio/PipeWire hosts (`status.md:1090-1147`). Tray now uses `pactl`; ALSA plugin clutter was eliminated at the source rather than filtered. | `git mv` with `Status: Superseded` |
| `2026-04-29-drop-input-device-config-knob-v1.md` | CLOSE-Completed | Executed jointly with the pulseaudio-first plan; `[audio].input_device`, `fono use input`, and the wizard mic picker are all removed (`CHANGELOG.md:1140-1148`). | `git mv` with `Status: Completed` |
| `2026-04-29-empty-transcript-microphone-recovery-v2.md` | CLOSE-Completed | Shipped v0.3.6 (`CHANGELOG.md:1091-1108`; `status.md:1149-1191`). | `git mv` with `Status: Completed` |
| `2026-04-29-pulseaudio-first-microphone-enumeration-v1.md` | CLOSE-Completed | Shipped v0.3.6. | `git mv` with `Status: Completed` |
| `2026-04-29-silent-input-device-auto-recovery-v1.md` | CLOSE-Superseded-by-OTHER | The auto-fallback design was rejected; user delegation to the OS audio layer (PulseAudio + tray Microphone submenu) shipped instead. See `status.md:1090-1147` and the empty-transcript-recovery-v2 plan. | `git mv` with `Status: Superseded` |
| `2026-04-29-streaming-config-collapse-v1.md` | CLOSE-Completed | `[stt.cloud].streaming` and `[interactive].overlay` fields removed in v0.3.5 (`CHANGELOG.md:1188-1207`). | `git mv` with `Status: Completed` |
| `2026-04-29-waveform-overlay-v1.md` | CLOSE-Superseded-by-v2 | v2 is the executed plan; v0.6.0 ships the merged batch + VU-bar surface (`CHANGELOG.md:758-805`). | `git mv` with `Status: Superseded` |
| `2026-04-29-waveform-overlay-v2.md` | CLOSE-Completed | Shipped v0.6.0. | `git mv` with `Status: Completed` |
| `2026-04-30-fono-single-binary-size-v1.md` | CLOSE-Completed | Phases 1–3 shipped across v0.3.7/v0.4.0 (ksni tray, llama.cpp common-feature opt-out, `--gc-sections`, dynamic-glibc ship target). Phase 2.4 (static-musl) was formally **deferred**, recorded in CHANGELOG v0.4.0 *Deferred* + ADR 0022 amendment. The plan as written is therefore done; the deferral has its own breadcrumbs. | `git mv` with `Status: Completed` |
| `2026-04-30-llama-cpp-sys-2-strip-common.patch.md` | CLOSE-Completed | Patch upstreamed and consumed via fork (`status.md:974-986`, CHANGELOG v0.3.7 lines 966-981). | `git mv` with `Status: Completed` |
| `2026-05-02-fono-cpu-gpu-variants-v1.md` | CLOSE-Completed | All three slices shipped in v0.5.0 (`CHANGELOG.md:810-860`; `status.md:358-509`). | `git mv` with `Status: Completed` |
| `2026-05-02-fono-install-subcommand-v1.md` | CLOSE-Superseded-by-v3 | v3 is the executed iteration (ADR 0023). | `git mv` with `Status: Superseded` |
| `2026-05-02-fono-install-subcommand-v2.md` | CLOSE-Superseded-by-v3 | Same. | Same |
| `2026-05-02-fono-install-subcommand-v3.md` | CLOSE-Completed | Shipped in v0.5.0 (`CHANGELOG.md:862-877`; `status.md:511-538`). | `git mv` with `Status: Completed` |
| `2026-05-03-whisper-vulkan-prewarm-v1.md` | CLOSE-Completed | Shipped v0.6.0 (`CHANGELOG.md:740-754`; `status.md:317-356`). | `git mv` with `Status: Completed` |
| `2026-05-04-fono-prelaunch-ux-polish-and-smoke-tests-v1.md` | UNCLEAR | Targets pre-v0.7.0 polish + smoke-test matrix. Most UX items (wizard collapse, single hotkey, toggle/hold auto) shipped via v0.7.1 and v0.8.0, but the plan also enumerates a smoke-test matrix that is not visibly all implemented (no obvious CI job dedicated to user-journey smoke tests beyond `cloud-assistant`). Review with maintainer; likely 80% closeable with remaining items extracted into a follow-up. | Investigate |
| `2026-05-04-fono-public-launch-strategy-v1.md` | CLOSE-Superseded-by-OTHER | Superseded by the 2026-05-15 launch-strategy v3 (which folded the assistant + Wave 1/Wave 2 corrections). | `git mv` with `Status: Superseded` |
| `2026-05-12-2026-05-12-wizard-primary-provider-collapse-issue-9-v1.md` | CLOSE-Superseded-by-OTHER | Superseded by `2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md` (which absorbed issue #9 alongside #11). | `git mv` with `Status: Superseded` |
| `2026-05-13-2026-05-13-feature-request-template-redesign-v1.md` | KEEP-Active | Advisory doc-only plan; no `.github/` files modified yet per the plan's own statement. | Leave in place |
| `2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md` | CLOSE-Completed | Phases A–G shipped in v0.8.0 (`CHANGELOG.md:330-419`; `status.md:120-164`). The "v2 only, no v1 visible" oddity is because the prior revision was the `2026-05-12-wizard-primary-provider-collapse-issue-9-v1` plan listed above; v2 is the merged successor. | `git mv` with `Status: Completed` |
| `2026-05-13-v0.8.0-prerelease-ux-corrections-v1.md` | CLOSE-Completed | All five fixes (A1–A4 build flags, wizard UX 1–4) appear in the CHANGELOG `[0.8.0]` *Fixed* + *Changed* blocks (`CHANGELOG.md:281-419`). | `git mv` with `Status: Completed` |
| `2026-05-14-first-run-autostart-and-wizard-v1.md` | CLOSE-Superseded-by-v4 | Earlier revision. | `git mv` with `Status: Superseded` |
| `2026-05-14-first-run-autostart-and-wizard-v2.md` | CLOSE-Superseded-by-v4 | Same. | Same |
| `2026-05-14-first-run-autostart-and-wizard-v3.md` | CLOSE-Superseded-by-v4 | Same. | Same |
| `2026-05-14-first-run-autostart-and-wizard-v4.md` | CLOSE-Superseded-by-OTHER | The 2026-05-15 onboarding v2 plan explicitly supersedes v4's `setup_completed` strand (Tasks B1, B4, F3, F4), and the simpler `tts_configured` predicate replaced the lane-probe + sentinel architecture. The user-visible outcome (auto-start from `curl \| sh`, tray nudge) shipped in the 2026-05-15 unreleased entry. Remaining v4 strands (FHS install path, terminal-spawn helper) were either already covered by the v0.5.0 self-installer or absorbed into the simpler onboarding path. | `git mv` with `Status: Superseded` |
| `2026-05-14-google-chirp-stt-v1.md` | KEEP-Active | Plan header explicitly says "draft / proposed, not scheduled". | Leave in place |
| `2026-05-14-kokoro-local-and-cloud-parity-v1.md` | KEEP-Active | Explicitly a future-work tracker, referenced from CHANGELOG `[Unreleased]` (`:209-210`). | Leave in place |
| `2026-05-14-openrouter-gemini-tts-optin-v1.md` | UNCLEAR | The kokoro-routing plan documents that `google/gemini-3.1-flash-tts-preview` **does not exist on OpenRouter today**, undermining the entire premise. Either close as `Abandoned` until OpenRouter actually exposes the model, or rewrite to target a different provider. Decision needed. | Investigate |
| `2026-05-14-openrouter-kokoro-multilingual-voice-routing-v1.md` | UNCLEAR | OpenRouter TTS default has since moved off Kokoro (`hexgrad/kokoro-82m`) twice: first to `openai/gpt-4o-mini-tts-…` then to `openai/tts-1` (`CHANGELOG.md:67-84, 199-210`). The voice-routing plan only matters if a user explicitly pins Kokoro. The kokoro-parity plan (`2026-05-14-kokoro-local-and-cloud-parity-v1.md`) absorbs the routing question as a sub-task. Likely **MERGE-INTO** `kokoro-local-and-cloud-parity-v1`. | See Recommended merges §3 |
| `2026-05-14-openrouter-tts-swap-to-openai-mini-v1.md` | CLOSE-Completed | Every task is already `[x]` checked in the plan body; the swap landed (`CHANGELOG.md:199-210`). Note the model was later changed again (to `tts-1`), but this plan's "swap from Kokoro to GPT-4o Mini TTS" objective shipped exactly. | `git mv` with `Status: Completed` |
| `2026-05-15-fono-public-launch-strategy-v1.md` | CLOSE-Superseded-by-v3 | Same author, same week, three revisions. | `git mv` with `Status: Superseded` |
| `2026-05-15-fono-public-launch-strategy-v2.md` | CLOSE-Superseded-by-v3 | Same. | Same |
| `2026-05-15-fono-public-launch-strategy-v3.md` | KEEP-Active | The two-wave launch strategy is the current execution doc; v0.9 has not shipped. | Leave in place |
| `2026-05-15-local-stt-affordability-recalibration-v1.md` | CLOSE-Superseded-by-v4 | v4 is the executed iteration; Phase 0 (AC sweep + Vulkan follow-up) shipped today per `status.md:5-91`. | `git mv` with `Status: Superseded` |
| `2026-05-15-local-stt-affordability-recalibration-v2.md` | CLOSE-Superseded-by-v4 | Same. | Same |
| `2026-05-15-local-stt-affordability-recalibration-v3.md` | CLOSE-Superseded-by-v4 | v4's own "What changed since v3" section documents the supersede. | Same |
| `2026-05-15-local-stt-affordability-recalibration-v4.md` | KEEP-Active | Phase 0 done; Phase 1 (registry refit, predicate changes) pending. Battery half of Phase 0 also pending. | Leave in place |
| `2026-05-15-onboarding-auto-start-and-tray-left-click-v1.md` | CLOSE-Superseded-by-v2 | Earlier revision. | `git mv` with `Status: Superseded` |
| `2026-05-15-onboarding-auto-start-and-tray-left-click-v2.md` | CLOSE-Completed | All three pillars shipped today in CHANGELOG `[Unreleased]` (`:24-65`). | `git mv` with `Status: Completed` |
| `2026-05-15-overlay-screencast-script-v1.md` | CLOSE-Superseded-by-v2 | Earlier revision. | `git mv` with `Status: Superseded` |
| `2026-05-15-overlay-screencast-script-v2.md` | CLOSE-Completed | Shipped today (`CHANGELOG.md:12-22`; `status.md:92-100`). | `git mv` with `Status: Completed` |
| `2026-05-15-stt-within-list-language-confidence-rerank-v1.md` | KEEP-Active | New plan filed today, not yet executed; covers a real bug (in-allow-list reranking) the existing v0.3.x stickiness work explicitly didn't address. | Leave in place |

Active-after-triage count: **9** (`kokoro-local-and-cloud-parity`,
`google-chirp-stt`, `fono-auto-translation`, `feature-request-template-redesign`,
`fono-public-launch-strategy-v3`, `local-stt-affordability-recalibration-v4`,
`stt-within-list-language-confidence-rerank-v1`, plus two UNCLEAR slots:
`openrouter-gemini-tts-optin`, `prelaunch-ux-polish-and-smoke-tests`).
The `openrouter-kokoro-multilingual-voice-routing-v1` plan is recommended
for merge, not standalone keep.

## 2. Recommended close batch

Run from the repo root. Each `git mv` should be followed by editing the
target file to insert the appropriate `Status:` header on a new line just
under the title, per `plans/closed/README.md`.

### 2.1 `Status: Completed` (32 plans — work landed in CHANGELOG)

```sh
git mv plans/2026-04-27-fono-interactive-v6.md plans/closed/
git mv plans/2026-04-27-fono-self-update-v1.md plans/closed/
git mv plans/2026-04-27-local-stt-llm-resolution-v1.md plans/closed/
git mv plans/2026-04-28-2026-04-28-wizard-local-model-selection-v1.md plans/closed/
git mv plans/2026-04-28-doc-reconciliation-v1.md plans/closed/
git mv plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md plans/closed/
git mv plans/2026-04-28-linux-notification-unification-via-notify-send-v1.md plans/closed/
git mv plans/2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md plans/closed/
git mv plans/2026-04-28-multi-language-stt-no-primary-v3.md plans/closed/
git mv plans/2026-04-28-stt-hallucination-strip-and-llm-diff-log-v3.md plans/closed/
git mv plans/2026-04-28-stt-language-allow-list-v1.md plans/closed/
git mv plans/2026-04-28-streaming-feedback-and-truncation-fixes-v1.md plans/closed/
git mv plans/2026-04-28-streaming-rate-limit-controls-v1.md plans/closed/
git mv plans/2026-04-28-wave-2-close-out-v1.md plans/closed/
git mv plans/2026-04-28-wave-3-slice-b1-v1.md plans/closed/
git mv plans/2026-04-28-wave-3-slice-b1-thread-c-live-groq-v2.md plans/closed/
git mv plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md plans/closed/
git mv plans/2026-04-29-drop-input-device-config-knob-v1.md plans/closed/
git mv plans/2026-04-29-empty-transcript-microphone-recovery-v2.md plans/closed/
git mv plans/2026-04-29-pulseaudio-first-microphone-enumeration-v1.md plans/closed/
git mv plans/2026-04-29-streaming-config-collapse-v1.md plans/closed/
git mv plans/2026-04-29-waveform-overlay-v2.md plans/closed/
git mv plans/2026-04-30-fono-single-binary-size-v1.md plans/closed/
git mv plans/2026-04-30-llama-cpp-sys-2-strip-common.patch.md plans/closed/
git mv plans/2026-05-02-fono-cpu-gpu-variants-v1.md plans/closed/
git mv plans/2026-05-02-fono-install-subcommand-v3.md plans/closed/
git mv plans/2026-05-03-whisper-vulkan-prewarm-v1.md plans/closed/
git mv plans/2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md plans/closed/
git mv plans/2026-05-13-v0.8.0-prerelease-ux-corrections-v1.md plans/closed/
git mv plans/2026-05-14-openrouter-tts-swap-to-openai-mini-v1.md plans/closed/
git mv plans/2026-05-15-onboarding-auto-start-and-tray-left-click-v2.md plans/closed/
git mv plans/2026-05-15-overlay-screencast-script-v2.md plans/closed/
```

### 2.2 `Status: Superseded` (28 plans — older revisions or replaced designs)

```sh
# fono-interactive v1..v5 → superseded by v6
git mv plans/2026-04-27-fono-interactive-v1.md plans/closed/
git mv plans/2026-04-27-fono-interactive-v2.md plans/closed/
git mv plans/2026-04-27-fono-interactive-v3.md plans/closed/
git mv plans/2026-04-27-fono-interactive-v4.md plans/closed/
git mv plans/2026-04-27-fono-interactive-v5.md plans/closed/

# multi-language-stt-no-primary v1, v2 → superseded by v3
git mv plans/2026-04-28-multi-language-stt-no-primary-v1.md plans/closed/
git mv plans/2026-04-28-multi-language-stt-no-primary-v2.md plans/closed/

# stt-hallucination-strip v1, v2 → superseded by v3
git mv plans/2026-04-28-stt-hallucination-strip-and-llm-diff-log-v1.md plans/closed/
git mv plans/2026-04-28-stt-hallucination-strip-and-llm-diff-log-v2.md plans/closed/

# waveform-overlay v1 → superseded by v2
git mv plans/2026-04-29-waveform-overlay-v1.md plans/closed/

# client-server-wyoming v1 → superseded by v2
git mv plans/2026-04-29-2026-04-29-client-server-wyoming-and-native-v1.md plans/closed/

# silent-input-device + alsa-plugin-filter → superseded by pulseaudio-first OS-delegation
git mv plans/2026-04-29-silent-input-device-auto-recovery-v1.md plans/closed/
git mv plans/2026-04-29-alsa-plugin-filter-and-cache-v1.md plans/closed/

# fono-install-subcommand v1, v2 → superseded by v3
git mv plans/2026-05-02-fono-install-subcommand-v1.md plans/closed/
git mv plans/2026-05-02-fono-install-subcommand-v2.md plans/closed/

# original launch strategy → superseded by 2026-05-15 v3
git mv plans/2026-05-04-fono-public-launch-strategy-v1.md plans/closed/

# wizard primary-provider-collapse (issue #9) → superseded by 2026-05-13 v2
git mv plans/2026-05-12-2026-05-12-wizard-primary-provider-collapse-issue-9-v1.md plans/closed/

# first-run-autostart-and-wizard v1..v4 → v4 itself superseded by 2026-05-15 onboarding v2
git mv plans/2026-05-14-first-run-autostart-and-wizard-v1.md plans/closed/
git mv plans/2026-05-14-first-run-autostart-and-wizard-v2.md plans/closed/
git mv plans/2026-05-14-first-run-autostart-and-wizard-v3.md plans/closed/
git mv plans/2026-05-14-first-run-autostart-and-wizard-v4.md plans/closed/

# fono-public-launch-strategy 2026-05-15 v1, v2 → superseded by v3
git mv plans/2026-05-15-fono-public-launch-strategy-v1.md plans/closed/
git mv plans/2026-05-15-fono-public-launch-strategy-v2.md plans/closed/

# local-stt-affordability-recalibration v1..v3 → superseded by v4
git mv plans/2026-05-15-local-stt-affordability-recalibration-v1.md plans/closed/
git mv plans/2026-05-15-local-stt-affordability-recalibration-v2.md plans/closed/
git mv plans/2026-05-15-local-stt-affordability-recalibration-v3.md plans/closed/

# onboarding-auto-start-and-tray-left-click v1 → superseded by v2
git mv plans/2026-05-15-onboarding-auto-start-and-tray-left-click-v1.md plans/closed/

# overlay-screencast-script v1 → superseded by v2
git mv plans/2026-05-15-overlay-screencast-script-v1.md plans/closed/
```

### 2.3 `Status: Abandoned`

None recommended outright; see Open Questions for two candidates.

## 3. Recommended merges

### Merge 1 — `openrouter-kokoro-multilingual-voice-routing-v1` → `kokoro-local-and-cloud-parity-v1`

**Why.** The voice-routing plan was authored when Kokoro was the OpenRouter
TTS default. Default has since moved off Kokoro twice (`CHANGELOG.md:199-210`
then `:67-84`), so the routing question is now opt-in-only. The
`kokoro-local-and-cloud-parity-v1` plan already lists "Shared
`KokoroVoiceRouter` (consumed by both backends)" as Scope item 2 and
references the routing plan by filename, so the architectural intent is
already to fold them.

**What content to preserve when merging.**

- The full 54-voice / 9-locale table from the routing plan's Phase 1
  Task 1 description — copy verbatim into the parity plan's
  `KokoroVoiceRouter` task.
- The diagnosis section (why `af_heart` produces accented French) —
  preserve as the merged plan's *Background* paragraph; it's the
  clearest written explanation of the symptom.
- Drop the swap-candidate triage (Gemini Flash TTS / GPT-4o Mini)
  from the merged version — both have been settled separately.
- Drop the OpenRouter-only "Phase 1 Task 1: add a `kokoro_voice_map`
  module under `crates/fono-tts/`" wording; under the merged plan the
  router is shared between local and cloud Kokoro backends rather
  than colocated with the OpenRouter client.

After merge: `git mv` the routing plan into `closed/` with
`Status: Superseded` (merged into kokoro-parity).

## 4. Open questions

1. **`2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`**
   includes Slices 5–7 (Fono-native WebSocket protocol, tray-side
   "Discovered on LAN" polish) that are not visibly shipped. CHANGELOG
   speaks of `fono-net-codec` and the Fono-native protocol but the
   WebSocket client/server appears to remain stubbed. Either close as
   `Completed (Slices 1–4)` and file a new follow-up plan for
   Slices 5–7, or leave the v2 plan active and tick what landed.
   Recommendation: **close as Completed with a follow-up note**;
   re-file Slices 5–7 only when there is concrete user demand.

2. **`2026-05-04-fono-prelaunch-ux-polish-and-smoke-tests-v1.md`** —
   ~80% of the listed UX polish shipped through v0.7.x / v0.8.0, but the
   end-to-end smoke-test matrix the plan demands (a CI job exercising
   the new-user journey) doesn't appear in `.github/workflows/`.
   Decide: extract the unshipped smoke-test tasks into a tiny follow-up
   plan and close this one, or keep it active with a partial-completion
   header.

3. **`2026-05-14-openrouter-gemini-tts-optin-v1.md`** —
   `google/gemini-3.1-flash-tts-preview` was verified non-existent on
   OpenRouter by `2026-05-14-openrouter-kokoro-multilingual-voice-routing-v1.md`
   (lines 30–39). Either:
   - close as `Abandoned` until OpenRouter actually exposes that model id, or
   - rewrite the plan against a verified-available multilingual TTS model
     (e.g. direct Google TTS via `2026-05-14-google-chirp-stt-v1.md`).

4. **`2026-05-13-2026-05-13-feature-request-template-redesign-v1.md`** is
   advisory-only by its own statement. If the maintainer has accepted
   the recommendations and edited `.github/ISSUE_TEMPLATE/feature_request.md`,
   close as `Completed`; otherwise leave active.

5. **Note about `medium` / `medium.en` removal today.** No active plan
   directly assumes those registry entries exist, but the
   `local-stt-affordability-recalibration-v4` Phase 1 task list will
   want a Status amendment when it next runs (the v3 plan body
   referenced `medium`-tier scoring rows that no longer apply).

## 5. Roll-up

| Outcome | Plans |
|---|---:|
| CLOSE-Completed | 32 |
| CLOSE-Superseded-by-vN | 23 |
| CLOSE-Superseded-by-OTHER | 5 |
| MERGE recommended | 1 |
| KEEP-Active (definite) | 7 |
| UNCLEAR / human input needed | 3 |
| **Total** | **71** |

Post-action, `plans/` would shrink from 71 active files to **8–10**
files (the seven definite keepers plus whatever the three Open
Questions resolve to), with everything else preserved as institutional
record under `plans/closed/`.
