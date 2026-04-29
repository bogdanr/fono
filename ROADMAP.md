# Fono Roadmap

> One binary. Any desktop. Your voice, at the cursor.

Fono is an open-source (GPL-3.0) voice dictation tool for Linux — native, lightweight,
and privacy-first. No Electron. No Python. No WebKit. Press a hotkey, speak, and your
words land at the cursor in any app, on any desktop, X11 or Wayland.

Items move from **Planned** → **In progress** → **Shipped** as work lands.
For exact per-release details see [`CHANGELOG.md`](CHANGELOG.md).
The home page is [fono.page](https://fono.page).

---

## In progress

---

## Planned — next

### Automatic translation

> Speak in Romanian, type in English. Or any other pair. Without leaving your editor.

Fono will translate as it transcribes — the pipeline becomes
**STT → translate → cleanup → inject**, entirely in the background:

- **Any source/target language pair**, not English-only. When the target is English,
  Whisper's native translation mode and the Groq/OpenAI `/audio/translations` endpoint
  provide a zero-latency fast path.
- **Per-app rules.** A `[[context_rules]]` override lets you target a different language
  per application — translate to English in your code editor, keep the original in your
  chat app.
- **Batch and live parity.** Works the same whether you use push-to-talk or streaming
  live-dictation mode.
- **One-shot CLI.** `fono translate <text> --to <code>` pipes any text through the
  configured translator without touching audio capture.

### Polish the auto-update

`fono update` is already there. A few finishing touches remain to handle edge cases
gracefully.

---

## Planned — later

- **Network inference — your powerful machine does the thinking for all your devices.**
  Run the Fono server on your desktop; run a featherweight Fono client on every other
  computer on your local network. The client streams raw audio over the LAN; the server
  runs Whisper and the LLM cleanup. The result lands at the cursor on the client in
  near-zero CPU and RAM — even on a ten-year-old laptop. Every byte stays on your
  private network; nothing touches the cloud unless you explicitly configure a cloud
  provider on the server.

- **Whisper protocol support.** As a companion to network inference, Fono will speak the
  Whisper server protocol so it can act as a drop-in replacement for, or client of,
  any existing faster-whisper / whisper.cpp server deployment on your network. If you
  already have a GPU machine running a Whisper endpoint, Fono on your thin clients will
  just point at it.

- **Wake-word activation via openWakeWord.** Always-on hands-free mode: Fono idles with
  a tiny wake-word detector (powered by
  [openWakeWord](https://github.com/dscripka/openWakeWord)) using a fraction of one CPU
  core. Say the magic word, and Fono wakes up and starts dictating — no hotkey, no
  reaching for the keyboard. When you stop speaking it goes back to sleep. The
  wake-word runs locally; your audio never leaves the machine while idle.

- **Hover-context injection (experimental).** Fono will peek at what the cursor is
  hovering over and silently adjust the cleanup prompt before injecting. Hovering over
  a terminal? The LLM is told to format output as shell commands. Hovering over a code
  editor? It prefers identifier-style casing and avoids prose punctuation. This is
  exploratory — an experiment to see how much smarter dictation can get just from a
  window-class and cursor-position hint, with no extra effort from the user.

- **Local REST API + MCP server.** Fono already runs as a daemon with a Unix-socket
  IPC layer — every CLI subcommand (`fono toggle`, `fono history`, `fono use …`) is a
  client talking to it. The next step is exposing that same interface over HTTP and the
  [Model Context Protocol](https://modelcontextprotocol.io), so scripts, editor plugins,
  and AI coding assistants can drive Fono without any special tooling.

- **Better Wayland hotkeys.** Today on Wayland (KDE, GNOME, wlroots) you bind the
  hotkey through your compositor's own settings. Once the
  `org.freedesktop.portal.GlobalShortcuts` portal becomes universally available, Fono
  will register its hotkeys through it automatically — zero setup.

- **macOS.** Native menu-bar app, proper system integration, signed `.dmg` download.
- **Windows.** System-tray app, native installer.

---

## Shipped

Newest first. Each entry says which release carried it.

- **Smarter first-run setup.** The setup wizard now asks whether you dictate only in
  English or multiple languages, then recommends the best on-device speech model your
  hardware can comfortably run. Technical jargon (WER%, AVX2, Whisper) removed; plain
  language hardware summary with a single-line accelerator description (e.g. "CPU only
  (AVX2 + FMA) — fine for batch dictation; live mode best with tiny"). Model shortlist
  capped at 3 choices; `medium` retired in favour of `large-v3-turbo`. Live-mode
  recommendation calibrated against CPU-only vs. hardware-accelerated thresholds.
  — *v0.3.5, 2026-04-29.*

- **Configurable streaming cadence + 429 awareness.** Live preview cadence is now
  controlled by `interactive.streaming_interval` (seconds, default 1.0, valid range
  0.5–3.0). Values above 3.0 disable the preview lane so only VAD-boundary finalize
  requests are sent — recommended for free-tier cloud users with strict per-minute caps.
  When the cloud responds with HTTP 429, the log suggests bumping the interval to 2.0
  or higher. The overlay is also forced on whenever streaming is enabled, since it is
  the only feedback surface live previews have. — *[v0.3.3], 2026-04-28.*

- **Banned-language gate actually fires.** v0.3.1's wrong-language self-correction was
  correct in design but unreachable in practice: the cloud transcribe call wasn't asking
  for the detail that includes the detected language code, so the gate never noticed a
  mismatch. Now the request always asks for that detail, and the language name is
  normalised to its short code before checking against your configured list.
  — *[v0.3.2], 2026-04-28.*

- **Cold-start language self-correction.** When the cloud transcriber's first response
  of a session is in the wrong language (e.g. English audio flagged as Russian for an
  accented speaker), Fono retries against every language you've configured and picks the
  one Whisper was most confident about — instead of injecting the wrong-language text.
  The streaming overlay also stops briefly flashing wrong-language text before the
  corrected result arrives. — *[v0.3.1], 2026-04-28.*

- **Release-time cloud quality gate.** Before producing release artefacts, every tag
  now runs the full multilingual fixture set (English, Romanian, Spanish, French,
  Chinese) through Groq's cloud Whisper and refuses to publish if any fixture's verdict
  diverges from the committed baseline. Catches both our regressions and upstream
  provider changes within minutes of tagging. — *[v0.3.0], 2026-04-28.*

- **Cloud transcription that learns your language.** If your cloud provider occasionally
  mishears your accent (e.g. flags English as Russian), Fono self-corrects after the
  first mistake and gets it right from then on. Bilingual users can switch languages
  freely without any toggle. Setup automatically adds English alongside whatever other
  language you pick. — *[v0.3.0], 2026-04-28.*

- **Reliable AI cleanup.** Fixed a long-standing bug where the cleanup step would
  occasionally reply with a clarifying question instead of cleaning your dictation.
  Affected every cloud and local AI provider; the fix applies universally. Very short
  utterances (one or two words) now skip cleanup entirely, saving about half a second.
  — *[v0.3.0], 2026-04-28.*

- **Live dictation actually ships.** The streaming "see your words appear as you speak"
  mode was built but accidentally left out of the packaged binary. v0.2.2 turns it on
  by default. — *[v0.2.2], 2026-04-28.*

- **Tamper-proof self-update.** `fono update` now verifies every file it downloads
  against a published checksum, refuses to overwrite files installed by your system
  package manager, and accepts a custom install directory. — *[v0.2.2], 2026-04-28.*

- **Automated quality gate.** Every pull request now runs a real speech-recognition
  test against committed audio samples, so we catch accuracy regressions before they
  ship. — *[v0.2.2], 2026-04-28.*

- **Streaming dictation mode.** First version of the live overlay — see your words
  appear as you speak, not only after you stop. — *[v0.2.1], 2026-04-28.*

- **Pick your dictation languages.** Replace the single-language setting with a list.
  Whisper now constrains itself to the languages you actually speak, instead of guessing
  wrong. — *[v0.2.1], 2026-04-28.*

- **Overlay no longer steals keyboard focus** on X11 desktops. — *[v0.2.1],
  2026-04-28.*

- **One binary, full local stack.** Both Whisper (speech-to-text) and a small local LLM
  (cleanup) ship inside the same single executable, with optional GPU acceleration. No
  Python, no Node, no Electron. — *[v0.2.0], 2026-04-27.*

- **Local cleanup AI in the setup wizard.** First-run setup now offers an offline LLM
  that runs entirely on your machine, sized automatically to your hardware.
  — *[v0.2.0], 2026-04-27.*

- **Friendlier hotkeys.** F9 to toggle, F8 for push-to-talk — single keys, no awkward
  chords, no clashes with desktop shortcuts. — *[v0.2.0], 2026-04-27.*

- **First public release.** Press a hotkey, speak, see your words at the cursor. Works
  with on-device Whisper out of the box, or with Groq / OpenAI / Anthropic / Cerebras /
  Deepgram if you'd rather use the cloud. Tray icon, history of recent dictations,
  hot-swappable providers. — *[v0.1.0], 2026-04-25.*

[v0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
[v0.2.0]: https://github.com/bogdanr/fono/releases/tag/v0.2.0
[v0.2.1]: https://github.com/bogdanr/fono/releases/tag/v0.2.1
[v0.2.2]: https://github.com/bogdanr/fono/releases/tag/v0.2.2
[v0.3.0]: https://github.com/bogdanr/fono/releases/tag/v0.3.0
[v0.3.1]: https://github.com/bogdanr/fono/releases/tag/v0.3.1
[v0.3.2]: https://github.com/bogdanr/fono/releases/tag/v0.3.2
[v0.3.3]: https://github.com/bogdanr/fono/releases/tag/v0.3.3
