# Client / Server Networking — Wyoming First, Fono-Native Second

## Objective

Establish Fono's first network protocol so a thin client (laptop, tablet,
old box) can offload speech recognition (and eventually LLM cleanup) to a
Fono server elsewhere on the LAN. Wyoming is the chosen first protocol
because:

- It is the de-facto open standard for voice services (Home Assistant,
  Rhasspy, faster-whisper, whisper.cpp, Piper, openWakeWord); supporting
  it gives Fono massive ecosystem leverage in both directions on day one.
- Its framing is trivial (single JSON-line header + optional UTF-8 data
  body + optional binary payload) — a few hundred lines of Rust without
  pulling new crates.
- Its scope (STT / TTS / wake / VAD / audio transport / mic / snd /
  satellite) overlaps cleanly with the parts of Fono that have natural
  network analogues.

It is, however, **not sufficient on its own** for the full
[Network inference](../ROADMAP.md#network-inference) roadmap entry.
Wyoming has no event types for: LLM-cleanup request/response, target-app
context (window class / title for hover-context injection rules),
per-app rule synchronisation, history-row mirroring, remote
paste-shortcut hint, secrets / config push, tray-state mirroring. We
therefore land Wyoming in Slices 1–3 (interop + STT split) and then
layer a **Fono-native LAN protocol** in Slices 4–6 carrying the rest of
the pipeline. The two protocols share framing semantics (JSONL header
+ optional binary payload) so the codec layer is single-sourced.

LAN-only is the explicit v1 scope. No mDNS, no TLS, no WAN tunnelling —
documented as such in the ADR and surfaced in `fono doctor`. Auth is
an optional pre-shared bearer token; default bind is loopback unless the
user opts into RFC1918.

## Scope Decisions (binding)

- **Wyoming wire format implemented natively in Rust.** No FFI to the
  Python `wyoming` library. Header is a single `\n`-terminated JSON
  object; optional `data_length` UTF-8 chunk merged on top of `data`;
  optional `payload_length` raw-bytes payload. PCM in Wyoming is
  16-bit little-endian; Fono's internal pipeline is mono f32 — the
  codec converts at the protocol boundary, not in
  `fono-audio`/`fono-stt`.
- **Wyoming covers STT now, TTS/wake later.** TTS and wake-word are
  on the roadmap but tracked separately; this plan ships the STT
  client + STT server only, with the codec future-proofed for the
  rest.
- **Wyoming streaming uses `transcript-chunk` when the peer advertises
  `supports_transcript_streaming`; otherwise we fall back to a single
  `transcript` event.** Both lanes must be implemented since
  wyoming-faster-whisper only added chunk streaming recently.
- **Fono-native protocol mirrors Wyoming's framing** (same JSONL +
  payload codec), but with a disjoint event-type namespace prefixed
  `fono.*` so a stray Wyoming peer is rejected at handshake. Reuses
  the same `fono-net-codec` module to avoid two parsers.
- **LAN-only, RFC1918 default.** Bind defaults to `127.0.0.1`. If
  user sets `bind = "0.0.0.0"` we refuse to start unless the
  resolved socket address is in `10.0.0.0/8`, `172.16.0.0/12`,
  `192.168.0.0/16`, `169.254.0.0/16`, or loopback. Documented escape
  hatch: `[server].allow_public = true` with a `WARN` banner on
  startup.
- **Auth = optional bearer token in header.** Pre-shared, written to
  `~/.config/fono/secrets.toml` via a new `fono keys add wyoming` /
  `fono keys add network` flow. Tokens are 32 random bytes
  base64url-encoded; verified constant-time. Wyoming itself does not
  define auth — we attach it as an extra `auth` field on the first
  `describe` / handshake event (Wyoming peers ignore unknown fields,
  so this is forward-compatible with vanilla wyoming-faster-whisper
  when no auth is configured).
- **No new heavyweight deps.** `tokio` (already in workspace),
  `serde_json` (workspace), `bytes` (already pulled by `reqwest`).
  No `tonic`, no `prost`, no `wyoming` crate from crates.io
  (the only existing one is unmaintained and would force a license /
  maintenance conversation we don't need).
- **Crate layout.** Two new crates:
  - `fono-net-codec` — pure framing + event enums (Wyoming + Fono);
    no I/O; `#![no_std]`-friendly except `serde_json` brings `std`.
    Target: < 600 LoC, 95 % test coverage.
  - `fono-net` — async TCP listener + connector + auth +
    server/client adapters that bridge into existing `SpeechToText`,
    `StreamingStt`, and (future) `TextFormatter` traits. Optional
    Cargo features: `wyoming-client`, `wyoming-server`,
    `fono-client`, `fono-server`. The default `fono` binary
    enables `wyoming-client` + `fono-client`; the `fono` server
    binary (same executable, different subcommand) enables
    `wyoming-server` + `fono-server`. No separate binary —
    `fono serve wyoming` / `fono serve network` is the entry.

## Implementation Plan

### Slice 1 — Codec foundation (offline)

- [ ] Task 1.1 — Create `crates/fono-net-codec` workspace member; add
      to root `Cargo.toml`; SPDX header + `[lints]` workspace = true.
      Rationale: keeps the byte-level parser away from any I/O so
      it is fuzzable and reusable by both the Wyoming and Fono-native
      sides without circular deps.
- [ ] Task 1.2 — Implement `Frame { header: serde_json::Value,
      data: Option<Vec<u8>>, payload: Option<Bytes> }` with
      `Frame::write_async<W: AsyncWrite>` and
      `Frame::read_async<R: AsyncBufRead>` per the Wyoming spec
      (JSON line, then `data_length` UTF-8 bytes, then
      `payload_length` raw bytes). Property tests with `proptest` on
      round-trip; corner cases: zero-length data, zero-length
      payload, both, neither, malformed JSON, oversized headers,
      truncated streams, embedded `\n` in payload (must not leak).
- [ ] Task 1.3 — Define typed event enums for the Wyoming subset we
      care about: `audio-start`, `audio-chunk`, `audio-stop`,
      `describe`, `info`, `transcribe`, `transcript`,
      `transcript-start`, `transcript-chunk`, `transcript-stop`.
      Each variant is a thin `serde::Deserialize`/`Serialize` struct
      that maps onto the JSON header. Provide a single
      `WyomingEvent` enum + `from_frame(&Frame)` / `to_frame()`
      conversions.
- [ ] Task 1.4 — Define typed event enums for the Fono-native
      subset (Slice 4 will fill the data shapes; Slice 1 just
      reserves the namespace and tag handler). Reserved tags:
      `fono.hello`, `fono.bye`, `fono.cleanup-request`,
      `fono.cleanup-response`, `fono.cleanup-chunk`,
      `fono.history-append`, `fono.context`, `fono.error`.
- [ ] Task 1.5 — Add `Cargo.toml` dependency and `deny.toml` audit
      pass; verify `cargo deny check` is clean.

### Slice 2 — Wyoming STT client (Fono talks to existing servers)

- [ ] Task 2.1 — Create `crates/fono-net` workspace member with
      feature-gated modules. SPDX, lints, deny audit.
- [ ] Task 2.2 — Implement `WyomingStt` in `fono-net::wyoming::client`
      behind feature `wyoming-client`. Two trait impls:
      `SpeechToText` (one-shot — buffers PCM, sends
      `audio-start` → chunked `audio-chunk` → `audio-stop` →
      `transcribe` → awaits `transcript`) and `StreamingStt` (when
      `info` describes `supports_transcript_streaming = true`,
      forwards `transcript-chunk` as `UpdateLane::Preview` and
      the closing `transcript` as `UpdateLane::Finalize`).
- [ ] Task 2.3 — Wire into the STT factory at
      `crates/fono-stt/src/factory.rs`: new backend id
      `"wyoming"`, config block `[stt.wyoming] uri = "tcp://host:port"
      model = "..." language = "..."`. Connection per call (cheap
      on a LAN, simpler than a long-lived pool for v1); `prewarm()`
      sends `describe` to populate model capabilities and validate
      the endpoint is reachable.
- [ ] Task 2.4 — Register in `fono-core::providers` so
      `fono use stt wyoming` works and `fono doctor` lists it under
      Providers with `key-not-applicable` (auth is the optional
      bearer, not an API key).
- [ ] Task 2.5 — Add the `[stt.wyoming]` knobs to the wizard's
      cloud-stt picker as a third option ("Wyoming server on your
      LAN — point me at a faster-whisper container, etc.").
      Validate reachability with the same 5 s `describe` probe used
      for cloud key validation.
- [ ] Task 2.6 — Integration test in `crates/fono-net/tests/`: spin
      up an in-process tokio task that speaks the Wyoming subset
      enough to validate a `transcribe` round-trip with a tiny PCM
      buffer; assert the returned `Transcription.text` matches the
      mock server's canned response. No real Whisper needed.

### Slice 3 — Wyoming STT server (Fono serves Home Assistant et al.)

- [ ] Task 3.1 — Implement `WyomingServer` in
      `fono-net::wyoming::server` behind feature `wyoming-server`.
      `tokio::net::TcpListener`, one task per connection,
      bounded broadcast for backpressure. Accepts `describe` →
      replies with a synthesised `info` covering the active
      `WhisperLocal` model name, language list, version
      (`fono <crate version>`); accepts the audio stream + a
      `transcribe` and replies with `transcript` (and, when
      streaming is on, `transcript-start` / `transcript-chunk` /
      `transcript-stop`).
- [ ] Task 3.2 — Wire into the daemon: new `[server.wyoming]`
      config block (`enabled`, `bind`, `port` default 10300,
      `auth_token_ref`, `models`), spawned at daemon start when
      enabled. Emit a clear `INFO` line: `wyoming server: bound on
      192.168.1.10:10300 (model=whisper-small, auth=enabled)`.
- [ ] Task 3.3 — RFC1918 enforcement: refuse non-private binds
      unless `[server].allow_public = true`; warn on every
      startup when public bind is on. Reject connections whose
      `peer_addr()` is non-loopback when `bind = "127.0.0.1"`
      (defence-in-depth against confused proxies).
- [ ] Task 3.4 — New CLI: `fono serve wyoming [--bind ...] [--port
      ...] [--model ...]`. Idempotent — running `fono serve` while
      a daemon is up sends an IPC `Request::ToggleServer` to the
      existing daemon rather than starting a second one (matches
      the existing `fono use` reload semantics).
- [ ] Task 3.5 — `fono doctor` Server section: lists each enabled
      server (currently `wyoming` only), bind address, peer count,
      the warning if public-bind is on, and a "test from another
      machine" recipe.
- [ ] Task 3.6 — Documentation: new `docs/network.md` page covering
      both directions (Fono as Wyoming client; Fono as Wyoming
      server hosting whisper-rs); explicit "what other Wyoming
      services interop" matrix; `wyoming-faster-whisper` Docker
      compose example. Cross-link from
      [Whisper protocol](../ROADMAP.md#whisper-protocol-support)
      roadmap entry.
- [ ] Task 3.7 — Integration test: in-process Wyoming client (the
      one written in Task 2.6, reused) drives `WyomingServer`
      backed by the existing mock STT used by
      `crates/fono/tests/pipeline.rs`. Round-trip
      `audio-start`→`transcript`. Adds a streaming variant when
      Slice 2's chunked path is online.

### Slice 4 — Fono-native LAN protocol design + codec

- [ ] Task 4.1 — Author ADR `docs/decisions/0022-network-protocols.md`
      capturing the two-layer decision (Wyoming for STT/TTS/wake +
      audio satellite interop; Fono-native for LLM cleanup +
      history + per-app context + future hover-context payloads),
      with the rejection rationale for the three alternatives we
      considered: (a) gRPC, (b) HTTP/JSON REST, (c) extending
      Wyoming with private event types. Cross-link the
      Wyoming-only path so future contributors know why we
      didn't simply tunnel everything through Wyoming.
- [ ] Task 4.2 — Specify the Fono-native event set in
      `docs/network.md` and as typed structs in
      `fono-net-codec::fono`:
      `Hello { client_version, capabilities, auth_token? }`,
      `HelloAck { server_version, capabilities, session_id }`,
      `Bye { reason }`,
      `CleanupRequest { id, raw_text, language?, app_context? }`,
      `CleanupResponse { id, cleaned_text, source_backend }`,
      `CleanupChunk { id, delta }` (future, reserved),
      `HistoryAppend { id, raw, cleaned, app_context, ts }`,
      `Context { window_class?, window_title?, app_id? }`
      (carried alongside CleanupRequest for hover-context;
      reserved for the per-app rules feature),
      `Error { code, message, retryable }`,
      `Ping { nonce }` / `Pong { nonce }`.
- [ ] Task 4.3 — Auth negotiation: bearer token in the `Hello`
      `auth_token` field; constant-time compare against the
      server's stored token; on mismatch the server sends
      `Error { code: "AUTH" }` and closes. Token storage piggybacks
      on the existing `secrets.toml` infra
      (`crates/fono-core/src/secrets.rs`).
- [ ] Task 4.4 — Capability negotiation: `Hello.capabilities` is a
      `Vec<String>` advertising what each side supports
      (`["cleanup", "history-mirror", "context"]`). Lets future
      slices add capabilities without breaking peers.

### Slice 5 — Fono-native client (network-mode dictation)

- [ ] Task 5.1 — Implement `NetworkLlm` in
      `fono-net::fono::client::llm` behind feature `fono-client`,
      implementing the existing `TextFormatter` trait. One TCP
      connection per cleanup, plus an opt-in pooled mode for
      latency-sensitive setups (deferred until it is shown to
      matter — round-trip on LAN is < 5 ms).
- [ ] Task 5.2 — Daemon profile flip: new `fono use mode network
      --server tcp://host:port [--token-name ...]` that:
      (a) sets `[stt].backend = "wyoming"` with the same `uri`,
      (b) sets `[llm].backend = "network"` with the matching
      `uri`,
      (c) leaves hotkey, audio capture, injection, tray, history
      DB on the client where the user is sitting.
- [ ] Task 5.3 — `fono doctor` Network section: client-side rows
      ("connected to <server>, RTT 1.4 ms, capabilities=[…],
      last cleanup=120 ms ago"). On disconnect, surface a
      desktop toast and degrade gracefully (the existing
      "skip cleanup" fallback when `Llm` errors already exists at
      `crates/fono/src/session.rs`).
- [ ] Task 5.4 — Integration test: in-process server task hosts a
      mock cleanup stub; a client `NetworkLlm.format(...)` round-
      trips through TCP loopback; assert `cleaned_text` matches
      and history mirrors when `history-mirror` capability is on.

### Slice 6 — Fono-native server (the inference box)

- [ ] Task 6.1 — Implement `FonoServer` in
      `fono-net::fono::server` behind feature `fono-server`. Routes
      `CleanupRequest` to the local `TextFormatter` (whichever the
      operator has configured — local llama, cloud Anthropic, etc.,
      transparent to the client). On `HistoryAppend` it writes
      into the *server's* history DB if `mirror_history = true`,
      otherwise drops; the *client's* history is the source of
      truth either way.
- [ ] Task 6.2 — CLI: `fono serve network [--bind ...] [--port
      ...] [--token-name ...]`. Co-resident with `fono serve
      wyoming`: a single daemon can host both servers
      simultaneously on different ports (`10300` Wyoming,
      `10301` Fono-native by default; document the choice).
- [ ] Task 6.3 — RFC1918 enforcement reuses Slice 3 Task 3.3 —
      shared helper `fono_net::bind::ensure_lan_only`.
- [ ] Task 6.4 — Docs: `docs/network.md` gains a "Run a Fono
      inference server" section with a complete Slackware /
      systemd recipe and the matching client-side
      `fono use mode network` flow.

### Slice 7 — Cross-cutting polish

- [ ] Task 7.1 — Tray surface: when running as a server, the tray
      icon gains a "Server: Wyoming + Fono on :10300/:10301
      (3 clients)" line. When the client is in network mode, the
      header reads "Network mode → 192.168.1.10".
- [ ] Task 7.2 — `fono hwprobe` extension: prints "this machine
      would make a good Fono server (X cores, Y GB RAM, AVX2)" or
      "this machine would make a good thin client (low CPU,
      offload recommended)" derived from existing
      `HardwareSnapshot` thresholds.
- [ ] Task 7.3 — Equivalence harness extension: new
      `fono-bench equivalence --stt wyoming --uri tcp://...`
      arm exercises the full Wyoming round-trip against the
      committed baseline so a server-side regression is caught
      by CI when the operator runs it pre-release.
- [ ] Task 7.4 — `CHANGELOG.md` `[Unreleased]` entries
      (Added: Wyoming STT client + server; Added: Fono-native LAN
      client + server; Added: `fono serve` subcommand;
      Added: `fono use mode network`).
- [ ] Task 7.5 — `ROADMAP.md` updates: move the Whisper protocol
      entry from "On the horizon" into "In progress" once Slice 3
      lands; move Network inference once Slice 6 lands.
- [ ] Task 7.6 — `docs/status.md` session entries at each slice
      boundary per the AGENTS.md rule.

## Verification Criteria

- Running `wyoming-faster-whisper` (the upstream Docker image) in a
  sibling container, `fono record --stt wyoming --uri tcp://...`
  produces a transcript indistinguishable from `fono record --stt
  groq` on the same fixture (within the existing levenshtein
  tolerance used by `fono-bench equivalence`).
- Running `fono serve wyoming` on machine A, then pointing Home
  Assistant's Wyoming integration at `tcp://A:10300`, transcribes a
  voice command end-to-end with the committed test fixture.
- Running `fono serve network` on machine A and `fono use mode
  network --server tcp://A:10301` on machine B, pressing F8 on B
  produces a cleaned transcript at the cursor on B with audio,
  STT, and LLM cleanup all having executed on A. Client-side CPU
  during a 10 s utterance does not exceed 10 % of one core
  (mic capture + injection + framing only).
- `fono doctor` cleanly reports server bind, RTT, peer count,
  and the LAN-only enforcement banner; flips to a critical
  warning when `allow_public = true`.
- `cargo deny check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, and `tests/check.sh` (full matrix) all green
  including the new feature flags.
- Codec property tests cover at least: round-trip of all defined
  Wyoming and Fono events; truncation, malformed JSON, payload
  embedding `\n`, oversized header rejection.
- Public-bind reject path has a unit test asserting that
  `bind = "0.0.0.0"` plus a public IP on the resolved interface
  refuses to start.

## Potential Risks and Mitigations

1. **Wyoming streaming compatibility drift.** The
   `transcript-chunk` event was added to the spec recently and
   not every faster-whisper deployment supports it.
   Mitigation: detect via `info.asr.supports_transcript_streaming`
   and fall back to the single-`transcript` lane; integration test
   hits both code paths.
2. **PCM format mismatch.** Wyoming defaults to 16-bit LE PCM;
   Fono is mono f32 internally. Naive conversion can drop dynamic
   range or clip on resample.
   Mitigation: convert at the codec boundary using the existing
   `fono-audio` resampler (`rubato`) and a saturating int16
   quantiser; unit-tested with sine-tone fixtures.
3. **LAN exposure footgun.** A user setting `bind = "0.0.0.0"`
   on a coffee-shop Wi-Fi instantly exposes their mic-equivalent
   to anyone on the subnet.
   Mitigation: RFC1918-only default; `allow_public` gate with a
   loud `WARN` on every daemon start; `fono doctor` flags it
   critically; documentation explicitly recommends a WireGuard
   tunnel for off-LAN access (not in scope for v1).
4. **Auth token leakage.** Putting a bearer token in the first
   `Hello` event means it lives in any peer's logs that captures
   the connection prologue.
   Mitigation: tokens are scoped per server and rotatable via
   `fono keys remove network && fono keys add network`; `fono
   doctor` does not print them; the daemon never logs them.
   Future TLS work (out of scope) will move to mutual auth.
5. **Two protocols, one codec — accidental Fono event leakage to a
   Wyoming peer.** A vanilla wyoming-faster-whisper would cope
   (unknown events ignored), but a strict implementation might
   close the connection.
   Mitigation: connection arms (`WyomingClient`, `WyomingServer`,
   `FonoClient`, `FonoServer`) are separate types holding
   separate event-type allow-lists; the codec rejects events
   outside the arm's allow-list at parse time.
6. **Per-connection-per-call overhead.** Opening a TCP connection
   per cleanup is fine on LAN but adds 1–2 ms.
   Mitigation: defer connection pooling to a follow-up plan only
   if a profiling pass against `fono-bench` shows it matters.
7. **Wyoming spec is a moving target.** New event types or fields
   ship without semver.
   Mitigation: the codec already handles unknown fields via
   `serde(other)` and unknown events via a fallback `Frame`
   variant; we track the upstream README in the ADR with the
   commit SHA we built against and bump on changelog mentions.
8. **License audit.** No new crates expected, but if any are
   added (e.g. `bytes` if it isn't already transitively pulled
   into our slim build), `deny.toml` must be updated per AGENTS.md.
   Mitigation: explicit Task 1.5 + Task 2.1 deny-audit gates.

## Alternative Approaches

1. **Wyoming-only, extend with private event types for cleanup +
   history.** Trade-off: single protocol for the user to reason
   about; downside is we'd be unilaterally extending an open
   standard, fragmenting interop with Home Assistant peers, and
   the private types are lost the moment the user points a real
   Home Assistant satellite at us. Rejected.
2. **gRPC + protobuf for the Fono-native protocol.** Trade-off:
   strong typing, codegen, streaming for free; downside is
   `tonic`/`prost` adds ~3 MB of binary and a build-time
   `protoc` dep that conflicts with the single-binary stance,
   and the protocol surface is small enough that JSONL is
   cheaper to iterate on than a `.proto` schema. Rejected for
   v1; revisitable if the protocol exceeds ~30 event types.
3. **HTTP/JSON REST + Server-Sent Events.** Trade-off: trivial
   to debug with `curl`; downside is bidirectional streaming on
   one connection is awkward (SSE is one-way), and we'd lose
   the Wyoming codec reuse. Rejected.
4. **Defer Fono-native protocol entirely; ship Wyoming only and
   accept that LLM cleanup runs client-side.** Trade-off:
   smallest scope for v1, ships sooner, real users get Wyoming
   interop today; downside is Network inference roadmap entry
   is only half-delivered (heavy local LLM still on the client).
   This is a reasonable cut-line if Slices 4–6 slip — the
   plan is structured so Slices 1–3 are independently
   shippable as a "v0.4 Wyoming interop" release, with Slices
   4–6 landing as "v0.5 Network inference".
5. **Drop client connection-per-call; mandate a single
   long-lived connection.** Trade-off: lower latency and easier
   observability; downside is reconnect logic, head-of-line
   blocking on a single mux, and worse failure isolation for
   the very common case of one cleanup per minute. Deferred to
   a follow-up that profiles against real workloads.
