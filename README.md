# Fono — Lightweight Native Voice Dictation

**Status: Work in progress (v0.1 scaffold).** The repository is scaffolded per
`docs/plans/2026-04-24-fono-design-v1.md`; no runtime functionality has landed
yet. Watch the CHANGELOG for v0.1.0.

Fono is a GPL-3.0, single-binary voice-dictation tool written in Rust. It
targets Linux first (X11 and Wayland, on lightweight distros like NimbleX,
Alpine, Void, Artix as well as KDE/GNOME) and ships with Windows and macOS as
follow-on targets. One statically-linked `fono` binary replaces the heavy
Tambourine (Tauri + Python) and OpenWhispr (Electron) stacks while delivering
the feature union of both.

## Roadmap

| Version | Scope |
|---------|-------|
| v0.1 | Hotkey → STT → LLM cleanup → paste; tray; history; personal dictionary; per-app context |
| v0.2 | Meeting transcription with on-device speaker diarization |
| v0.3 | Notes store with folders + semantic search |
| v0.4 | Local REST API + MCP server |

## Design

The full design lives at
[`docs/plans/2026-04-24-fono-design-v1.md`](docs/plans/2026-04-24-fono-design-v1.md).
Architecture notes, provider matrix, Wayland caveats, and privacy posture will
accrue under [`docs/`](docs/) as phases land.

## Workspace layout

```
crates/
├── fono            # bin: entry point, CLI, first-run wizard
├── fono-core       # lib: config, errors, DB schema, paths
├── fono-audio      # lib: cpal capture, VAD, resampling
├── fono-stt        # lib: STT trait + local + cloud backends
├── fono-llm        # lib: LLM trait + local + cloud backends
├── fono-hotkey     # lib: global-hotkey wrapper + hold/toggle FSM
├── fono-inject     # lib: enigo + Wayland fallback (wtype/ydotool)
├── fono-tray       # lib: tray-icon wrapper, menu
├── fono-overlay    # lib: minimal winit/softbuffer recording indicator
├── fono-ipc        # lib: Unix-socket IPC between daemon and CLI
└── fono-download   # lib: HuggingFace model downloader w/ progress
```

## Building (once Phase 1+ lands)

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md). All commits **must** carry a
`Signed-off-by:` trailer (`git commit -s`) per the Developer Certificate of
Origin; CI rejects PRs that are missing it.

## License

Fono is distributed under the terms of the **GNU General Public License,
version 3 only**. See [LICENSE](LICENSE) for the full text.
