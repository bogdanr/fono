# ADR 0002 — Project name: Fono

## Status

Accepted 2026-04-24.

## Context

The project needs a short, typable, globally pronounceable, collision-free name that
will serve as the binary name, the crate name, the systemd unit name, and the config
directory name. The name must survive shell escaping, filesystem portability, and
package-registry collision checks.

## Decision

**Fono** — from Greek *φωνή* ("voice") and the Romance-language *phono-* root.

- Binary: `fono`
- Crate: `fono`
- Config dir: `~/.config/fono/`
- Systemd unit: `fono.service`

## Consequences

### Positive

- 4 letters — short, muscle-memory friendly.
- Typeable without modifier keys on any standard keyboard layout.
- Pronounceable in English, Romanian, Italian, Spanish, Portuguese, and German.
- No major Linux package, Cargo crate, or registered trademark collision was found at
  the time of selection.
- ASCII-only — safe for all shells, filesystems, archive formats, and CI runners.

### Negative

- Slightly generic; the name will require active discoverability work (website, clear
  README SEO, tagline) as the project grows.

## Alternatives rejected (with reasons)

- **murmur** — collides with the Mumble VoIP server binary `murmurd`.
- **whispr / whiskr** — Whisper / WisprFlow / OpenWhispr already crowd this root.
- **quill** — QuillJS (huge rich-text editor) plus a dozen note-taking apps.
- **babel** — Babel JS compiler dominates the search space.
- **aria** — Opera's AI assistant, plus the ARIA PGP cipher.
- **tonto** — problematic slang in Spanish, plus Lone Ranger racial baggage.
- **logos** — overloaded (religious connotation + Logos Bible Software).
- **vox / dict / speak** — too generic; many collisions (`dictd`, `espeak`, etc.).
- **oratio, prose, susurro, loqui, parola, verba, steno, canto, lingo, scribo** —
  all considered, all technically viable; Fono chosen by maintainer preference.
