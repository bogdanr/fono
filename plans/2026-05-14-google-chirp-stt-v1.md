# Plan: Google Cloud Speech (Chirp 3) — STT + TTS, languages + voices

*Created 2026-05-14. Owner: TBD. Status: **draft / proposed**, not scheduled.*

## Context

In v0.8.x we wired **OpenRouter STT** so the wizard's primary-collapse
path automatically configures `openai/whisper-large-v3-turbo` (routed
to Groq Whisper) when the user picks OpenRouter as their primary
provider. OpenRouter also advertises `google/chirp-3` on
`POST /v1/audio/transcriptions`, but the route is a thin proxy with
known limitations:

- No per-utterance language hint (Chirp's strength).
- No phrase-set / boost lists, no diarisation, no word-timing offsets.
- No streaming.
- Charged per-second at OpenRouter's markup over Google's own price.

Routing Chirp through OpenRouter therefore loses the very features
that make Chirp 3 distinctive. The user's intent — *real* Chirp 3,
with language and voice support, and the matching Chirp TTS — needs
a first-class Google Cloud Speech client.

## Scope

1. **STT path** — `speech.googleapis.com/v2/projects/{project}/locations/{loc}/recognizers/{recognizer}:recognize` (synchronous batch) for the canonical batch flow, and `:streamingRecognize` (bidi gRPC) for live-dictation parity with Groq streaming.
2. **TTS path** — `texttospeech.googleapis.com/v1beta1/text:synthesize` with the Chirp 3: HD voice family (`en-US-Chirp3-HD-Charon` and siblings).
3. **Auth** — Google Cloud uses **OAuth 2 service-account JWTs**, *not* bearer API keys. Need:
   - A new `secrets.toml` entry shape: `GOOGLE_APPLICATION_CREDENTIALS = "/path/to/service-account.json"` (the de-facto industry env var name; Google's own SDKs honour it).
   - A tiny JWT signer (or `gcp_auth` / `yup-oauth2` crate, MIT/Apache-2 compatible with GPL-3) that mints access tokens from the service account, caches them until ~5 min before expiry.
4. **Wizard** — promote `google` from STT-only stub to a real catalogue entry advertising `stt: Some(...)` *and* `tts: Some(...)`. The primary picker should mark Chirp 3 with the `Stt + Tts + Vision-not-applicable` capability set.
5. **Languages** — Chirp 3 supports 100+ locales. The wizard's language picker (`pick_languages` in `crates/fono/src/wizard.rs`) currently lists a hand-curated set; extend it to surface a "more languages…" Chirp-3-specific submenu so the user can pin e.g. `ar-XA`, `cmn-CN`, `hi-IN`, `pl-PL` without hand-editing config.
6. **Voices** — Chirp 3 HD ships ~30 named voices per locale (`Charon`, `Aoede`, `Fenrir`, `Kore`, `Leda`, `Orus`, `Puck`, `Zephyr`, …). Add a `[tts.voice]` picker for Google specifically that surfaces the named voices for the configured locale, rather than the free-form string field every other backend uses.

## Out of scope (this plan)

- Vertex AI's enterprise endpoint (`{region}-aiplatform.googleapis.com`). Standard Speech v2 covers all features the dictation UX needs.
- gRPC dependency. Start with REST/JSON for the batch path; reach for gRPC only for the streaming path, behind an `accel-grpc`-style feature flag so the default build stays slim (tonic + prost adds ~6 MB stripped).

## Implementation outline

1. **`crates/fono-net-google/`** *(new crate)* — owns the service-account
   JWT minting and OAuth token cache. Re-exported from `fono-stt` and
   `fono-tts` to avoid duplicate code.
2. **`crates/fono-stt/src/google.rs`** — REST client for the batch
   `:recognize` path; streaming variant lands later behind a `google-streaming` feature.
3. **`crates/fono-tts/src/google.rs`** — REST client for `:synthesize`,
   Chirp 3 HD voice catalogue baked in as a `&'static [&'static str]`
   keyed by locale.
4. **Catalogue update** — `crates/fono-core/src/provider_catalog.rs`'s
   `google` entry gains `stt: Some(SttDefaults { model: "chirp-3" })`
   and `tts: Some(TtsDefaults { model: "chirp-3-hd", default_voice: "en-US-Chirp3-HD-Charon", endpoint: TtsEndpoint::Google { ... }, runtime_probe: false })`. Add a `TtsEndpoint::Google` variant.
5. **Defaults** — `crates/fono-stt/src/defaults.rs::default_cloud_model("google")` from `"default"` → `"chirp-3"`.
6. **Wizard** — Google joins the primary-candidate set; when picked, the wizard runs the **service-account JSON picker** flow (browse / paste path, validate by minting a token against `https://oauth2.googleapis.com/token`), then the **locale picker** with Chirp 3's locale list, then the **voice picker** with Chirp 3's per-locale voice list.
7. **Doctor** — `fono doctor` learns to mint a fresh access token via the service account and confirms `speech.googleapis.com` reachability.

## Risks / open questions

- **GPL-3 compatibility of `gcp_auth`**: dual MIT/Apache-2, fine. `yup-oauth2`: MIT, fine. Either works; pick the smaller dep tree at implementation time.
- **Service-account JSON in `secrets.toml`**: we currently store **bearer strings**, not file paths. Two options: (a) keep the SA JSON on disk, store only the path; or (b) inline the JSON. (a) matches Google's own conventions; (b) keeps everything in one file. Decision deferred to implementation.
- **Free tier**: Google Cloud Speech has a 60-min/month free tier; perfect for evaluation. Document this in `docs/providers.md` when shipping.
- **NimbleX packaging**: no new system dep beyond what we already vendor (rustls, etc.). `gcp_auth` is pure Rust.

## Acceptance criteria

- `fono setup` lets the user pick "Google Cloud Speech" as a primary provider, point at a service-account JSON, pick a locale and a Chirp 3 voice, and end up with a working STT + TTS round-trip on first run.
- `fono use stt google` / `fono use tts google` set the right backend with the catalogue defaults, mirroring the existing Groq / OpenAI flow.
- `fono doctor` reports green when the service-account JSON is reachable and `speech.googleapis.com` returns an access token.
- The new crate adds ≤ 4 transitive dependencies and ≤ 800 KB to the stripped release binary.
