# Fono Roadmap

Fono is a tiny voice-dictation app for Linux: press a hotkey, speak,
and your words land at the cursor — in any window, any app. This page
is a plain-English view of where Fono is going. Items move from
**Planned** → **In progress** → **Shipped** as work lands.

For exact per-release details, see [`CHANGELOG.md`](CHANGELOG.md).
The home page is [fono.page](https://fono.page).

---

## In progress

- **Smarter first-run setup.** Ask one question — *"Will you dictate
  only in English, or in multiple languages?"* — then suggest the
  best on-device speech model your computer can comfortably run, with
  an honest accuracy estimate for each language you picked.
  English-only models are smaller and more accurate per megabyte, so
  English-only users get a better experience automatically.

## Planned — next

- **Automatic translation.** Speak in one language, get text in
  another. Works in both directions, with per-app rules (e.g. always
  translate to English when typing in your code editor, but keep the
  original language in your chat app).
- **Polish the auto-update.** "Fono, update yourself" is mostly there
  already — finishing touches to handle a few edge cases gracefully.

## Planned — later

- **macOS support.** Native menu-bar app, proper system integration,
  signed `.dmg` download.
- **Windows support.** System-tray app, native installer.
- **Better Wayland hotkeys.** Today on Wayland (KDE, GNOME) you have
  to bind the hotkey through your desktop's settings. Once Linux
  desktops finish shipping the new shared-shortcut standard, Fono will
  pick it up automatically with no setup.
- **Live cleanup as you speak.** Today the AI cleanup (punctuation,
  capitalisation, removing filler words) runs once when you stop
  speaking. Doing it gradually, while you're still talking, would feel
  more responsive.

## Won't do (for now)

- **No tracking, no analytics.** Fono will never phone home. See
  [`docs/privacy.md`](docs/privacy.md).
- **No "default" models with restrictive licences.** Models from
  Meta's Llama or Google's Gemma families are available if you opt in,
  but won't be downloaded by default — their licences aren't
  open-source-approved. See ADR
  [`0004-default-models.md`](docs/decisions/0004-default-models.md).
- **No web or Electron interface.** The whole point of Fono is to
  stay small and native. The tray icon, the floating overlay, and the
  command line are the only interfaces.

---

## Shipped

Newest first. Each entry says which release carried it.

- **Configurable streaming cadence + 429 awareness.** Live preview
  cadence is now controlled by `interactive.streaming_interval`
  (seconds, default 1.0, valid range 0.5-3.0). Values above 3.0
  disable the preview lane entirely so only VAD-boundary finalize
  requests are sent — recommended for free-tier cloud users with
  strict per-minute caps. When the cloud responds with HTTP 429 the
  log now suggests bumping the interval to 2.0 or higher. The overlay
  is also forced on whenever streaming is enabled, since it's the
  only feedback surface live previews have. — *v0.3.3, 2026-04-28.*

- **Banned-language gate actually fires.** v0.3.1's wrong-language
  self-correction was correct in design but unreachable in practice:
  the cloud transcribe call wasn't asking for the detail that includes
  the detected language code, so the gate never noticed a mismatch.
  Now the request always asks for that detail, and the language name
  is normalised to its short code before checking against your
  configured list. — *v0.3.2, 2026-04-28.*

- **Cold-start language self-correction.** When the cloud transcriber's
  first response of a session is a wrong language (e.g. English audio
  flagged as Russian for an accented speaker), Fono now retries against
  every language you've configured and picks the one Whisper was most
  confident about — instead of injecting the wrong-language text. The
  streaming overlay also stops briefly flashing wrong-language text
  before the corrected result arrives. — *v0.3.1, 2026-04-28.*

- **Release-time cloud quality gate.** Before producing release
  artefacts, every tag now runs the existing multilingual fixture set
  (English, Romanian, Spanish, French, Chinese) through Groq's cloud
  Whisper and refuses to publish if any fixture's verdict diverges
  from the committed baseline. Catches both our regressions and
  upstream provider changes (schema drift, model deprecations) within
  minutes of tagging. — *v0.3.0, 2026-04-28.*
- **Cloud transcription that learns your language.** If your cloud
  provider occasionally mishears your accent (e.g. flags English as
  Russian), Fono now self-corrects after the first mistake and gets
  it right from then on. Bilingual users can switch languages freely
  without any toggle. Setup automatically adds English alongside
  whatever other language you pick. — *v0.3.0, 2026-04-28.*
- **Reliable AI cleanup.** Fixed a long-standing bug where the cleanup
  step would occasionally reply with a question ("Could you provide
  the full text?") instead of cleaning your dictation. Affected every
  cloud and local AI provider; the fix applies universally. Very short
  utterances (one or two words) now skip cleanup entirely, saving
  about half a second. — *v0.3.0, 2026-04-28.*
- **Live dictation actually ships.** The streaming "see your words
  appear as you speak" mode was built but accidentally left out of the
  packaged binary. v0.2.2 turns it on by default. — *v0.2.2,
  2026-04-28.*
- **Tamper-proof self-update.** `fono update` now verifies every file
  it downloads against a published checksum, refuses to overwrite
  files installed by your system package manager, and accepts a
  custom install directory. — *v0.2.2, 2026-04-28.*
- **Automated quality gate.** Every pull request now runs a real
  speech-recognition test against committed audio samples, so we
  catch accuracy regressions before they ship. — *v0.2.2, 2026-04-28.*
- **Streaming dictation mode.** First version of the live overlay —
  see your words appear as you speak, not only after you stop. —
  *v0.2.1, 2026-04-28.*
- **Pick your dictation languages.** Replace the single-language
  setting with a list. Whisper now constrains itself to the languages
  you actually speak, instead of guessing wrong. — *v0.2.1,
  2026-04-28.*
- **Overlay no longer steals keyboard focus** on X11 desktops. —
  *v0.2.1, 2026-04-28.*
- **One binary, full local stack.** Both Whisper (speech-to-text) and
  a small local LLM (cleanup) ship inside the same single executable,
  with optional GPU acceleration. No Python, no Node, no Electron. —
  *v0.2.0, 2026-04-27.*
- **Local cleanup AI in the setup wizard.** First-run setup now
  offers an offline LLM that runs entirely on your machine, sized
  automatically to your hardware. — *v0.2.0, 2026-04-27.*
- **Friendlier hotkeys.** F9 to toggle, F8 for push-to-talk — single
  keys, no awkward chords, no clashes with desktop shortcuts. —
  *v0.2.0, 2026-04-27.*
- **First public release.** Press a hotkey, speak, see your words at
  the cursor. Works with on-device Whisper out of the box, or with
  Groq / OpenAI / Anthropic / Cerebras / Deepgram if you'd rather use
  the cloud. Tray icon, history of recent dictations, hot-swappable
  providers. — *v0.1.0, 2026-04-25.*

[v0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
[v0.2.0]: https://github.com/bogdanr/fono/releases/tag/v0.2.0
[v0.2.1]: https://github.com/bogdanr/fono/releases/tag/v0.2.1
[v0.2.2]: https://github.com/bogdanr/fono/releases/tag/v0.2.2
[v0.3.0]: https://github.com/bogdanr/fono/releases/tag/v0.3.0
