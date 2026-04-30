# Client / Server Networking — Wyoming + Fono-Native (WebSocket) + mDNS Discovery

> **Amendment 2026-04-29 (v2.1).** Fono-native protocol upgraded from a
> raw TCP transport to **WebSocket** so a browser tab can be a
> first-class Fono client without a protocol redesign. `tokio-tungstenite`
> is already in the workspace dependencies (used by Groq streaming) so
> this costs zero new deps. Concretely:
>
> - Wyoming stays as raw TCP (per its spec) on `:10300`. No change.
> - Fono-native runs over WebSocket on `:10301` at path `/fono/v1`. The
>   `Frame { header, data, payload }` codec defined in Slice 1 stays
>   identical; transport changes only at the I/O boundary. One Frame
>   serialises to one binary WebSocket message (header_len:u32 LE +
>   header_json + data_len:u32 LE + data + payload_len:u32 LE + payload),
>   keeping browser-side parsing trivial (one DataView read, one
>   JSON.parse, then two slice() calls).
> - mDNS TXT records gain a `path` key (`path=/fono/v1`) so a discovered
>   peer's full URL is constructible without a side-channel; clients
>   default to `path=/fono/v1` if the key is absent.
> - The `discovery` browser advertises `_fono._tcp.local.` (lowercase
>   `_tcp` is correct per RFC 6763 even when the underlying transport
>   is WebSocket — the TCP carrier of the upgrade hop). TXT
>   `proto=fono-ws` distinguishes from a hypothetical future raw-TCP
>   variant.
> - Auth token still travels in the first `fono.hello` event payload,
>   not in the WebSocket upgrade headers, so a browser client running on
>   a non-fono origin can supply the token without bumping into CORS or
>   custom-header restrictions.
>
> Slice 5 implementation tasks updated below to call out the WS
> transport explicitly. Slice 1 codec work is unchanged — `Frame` is
> transport-agnostic.

> **Amendment 2026-04-29 (v2.2 — Slice 2 crate placement).** The
> `WyomingStt` *client* implementation (Slice 2 Tasks 2.2 / 2.3) lives
> in `crates/fono-stt/src/wyoming.rs` as a feature-gated sibling of the
> existing `groq` / `openai` modules, depending directly on
> `fono-net-codec`. The originally-planned `crates/fono-net` crate
> would have created a dependency cycle (`fono-net` → `fono-stt` for
> the `SpeechToText` trait, `fono-stt` → `fono-net` for the factory
> dispatch). Putting the client where its peers live eliminates the
> cycle, mirrors the established convention, and keeps slim builds
> slim (the `wyoming` feature on `fono-stt` only pulls in the
> already-vendored `fono-net-codec`). The `fono-net` crate is still
> created — but in Slice 3, where it owns the *server* + discovery
> code that doesn't need the STT trait at all.

> **Supersedes** `plans/2026-04-29-2026-04-29-client-server-wyoming-and-native-v1.md`.
> Two design simplifications drove the v2 rewrite:
>
> 1. **There is no "client mode."** The default `fono` daemon is the
>    client. Network sources are simply two more backends in the same
>    `[stt].backend` / `[llm].backend` enums alongside `local`,
>    `groq`, `openai`, etc. No `fono use mode …` flag, no separate
>    profile, no parallel state machine.
> 2. **Autodiscovery is a first-class slice, not a follow-up.** mDNS
>    browsing happens automatically; discovered peers light up in the
>    tray STT/LLM submenus and are one-click selectable. Discovered
>    state is ephemeral (never persisted) — selecting a row promotes
>    it to the configured `[stt.wyoming].uri` / `[llm.fono].uri`.

## Objective

Ship Fono's first network protocols and zero-config LAN discovery so:

- Any `fono` install on the LAN **automatically sees** other Fono servers
  and Wyoming-compatible servers (faster-whisper, whisper.cpp, Piper,
  openWakeWord, Home Assistant satellites) without the user editing a
  config file or running a setup wizard.
- Any `fono` install can **be selected as a server** for other Fono
  installs by enabling `[server.wyoming]` and/or `[server.fono]` in its
  config — at which point it advertises over mDNS and accepts inbound
  STT and (for the Fono-native protocol) LLM-cleanup requests.
- The full network-inference roadmap entry is delivered without
  introducing a separate "thin client" concept: a low-end laptop just
  picks `Fono server · studio.local` from the tray and from then on
  acts as a thin client, even though the binary running on it is
  identical to the one running on the server.

Wyoming covers the audio/STT (and later TTS/wake) interop with the
Rhasspy / Home Assistant ecosystem. The Fono-native protocol covers
the parts Wyoming has no event types for (LLM cleanup, history mirror,
app-context routing). Both speak the same JSONL-header + binary-payload
codec so we ship one parser.

## Locked Decisions

- **No client/server "mode" flag.** Wyoming and Fono-native are
  backend rows like any other. `[stt].backend ∈ {local, groq, openai,
  wyoming, fono-server}`; `[llm].backend ∈ {local, groq, openai,
  cerebras, anthropic, openrouter, ollama, fono-server}`.
- **Discovered state is ephemeral.** mDNS results live in memory only,
  rebuilt on every daemon start. Clicking a discovered tray row
  promotes it into the persistent `[stt.<backend>]` block via the
  existing `Config::set_active_*` path.
- **Autodiscovery is always on.** The daemon browses mDNS while it is
  running, and enabled servers advertise themselves automatically. There is no
  user-facing discovery toggle; disabled servers do not advertise.
- **Pure-Rust mDNS via the `mdns-sd` crate.** No `avahi-client`
  /`bonjour` FFI. License: dual MIT/Apache-2.0 (GPL-3.0 compatible).
  Audit gate in Slice 1.
- **Service types**:
  - `_wyoming._tcp.local.` — Wyoming-protocol speakers (STT, TTS,
    wake; `proto=wyoming` TXT key disambiguates the role).
  - `_fono._tcp.local.` — Fono-native protocol.
  - TXT records (both): `proto`, `version`, `caps` (comma list),
    `auth` (`none`|`token`), `name` (human-readable instance name).
- **LAN exposure is controlled by `bind`.** Server binds default to loopback.
  Set `0.0.0.0` / `::` for all interfaces or a specific interface address for
  one NIC; Fono does not carry a separate public-bind override.
- **Auth = optional pre-shared bearer token in the first event**
  (`describe`/`Hello`). Stored via `fono keys add wyoming` /
  `fono keys add fono-server`. TXT advertises `auth=token` so the
  client can prompt for the token at first selection.
- **One new dep total: `mdns-sd ~0.11`.** No `tonic`, no `prost`, no
  `wyoming` crate from crates.io. `serde_json`, `tokio`, `bytes` are
  all already in the workspace.
- **Two new crates only**:
  - `fono-net-codec` — pure framing + event enums, no I/O, fuzzable.
  - `fono-net` — async TCP + mDNS + server/client adapters bridging
    into existing `SpeechToText` / `StreamingStt` / `TextFormatter`
    traits.
- **Cargo features keep the slim build slim.** Default `fono` build:
  `wyoming-client`, `fono-client`, `discovery`. The slim cloud-only
  build (`--no-default-features --features tray,cloud-all`) gets
  `wyoming-client` + `discovery` for free (cheap) but skips
  server-side code.

## Implementation Plan

### Slice 1 — Codec foundation (offline, no network)

- [ ] Task 1.1 — Create `crates/fono-net-codec` workspace member;
      SPDX header on every file; `[lints]` workspace = true; root
      `Cargo.toml` member list updated.
- [ ] Task 1.2 — Implement `Frame { header: serde_json::Value,
      data: Option<Vec<u8>>, payload: Option<Bytes> }` with
      `Frame::write_async<W>` / `Frame::read_async<R>` per the
      Wyoming spec (JSON line `\n` then `data_length` UTF-8 then
      `payload_length` raw bytes). Property tests round-trip every
      defined event; corner cases: zero-length data/payload,
      embedded `\n` in payload, malformed JSON, oversized header,
      truncated stream.
- [ ] Task 1.3 — Typed Wyoming event enum covering the STT subset
      (`audio-start`, `audio-chunk`, `audio-stop`, `describe`,
      `info`, `transcribe`, `transcript`, `transcript-start`,
      `transcript-chunk`, `transcript-stop`). Each variant is a
      thin `serde::Deserialize` struct mapping the JSON header.
- [ ] Task 1.4 — Typed Fono-native event enum reserving the
      namespace: `fono.hello`, `fono.hello-ack`, `fono.bye`,
      `fono.cleanup-request`, `fono.cleanup-response`,
      `fono.cleanup-chunk`, `fono.history-append`, `fono.context`,
      `fono.error`, `fono.ping`, `fono.pong`. Field shapes
      finalised in Slice 5; Slice 1 ships the tag enumeration
      and the round-trip test scaffold.
- [ ] Task 1.5 — Connection-arm types (`WyomingClient`,
      `WyomingServer`, `FonoClient`, `FonoServer`) each holding
      an event-tag allow-list enforced at parse time so a stray
      Fono event sent to a strict Wyoming peer is rejected before
      it reaches the wire.
- [ ] Task 1.6 — `cargo deny check` clean; `deny.toml` updated if
      `serde_json` or `bytes` need explicit pinning.

### Slice 2 — Wyoming STT client (Fono talks to existing servers)

- [ ] Task 2.1 — Create `crates/fono-net` workspace member,
      SPDX/lints/deny; feature flags `wyoming-client`,
      `wyoming-server`, `fono-client`, `fono-server`, `discovery`.
- [ ] Task 2.2 — Implement `fono_net::wyoming::client::WyomingStt`
      behind `wyoming-client`. Two trait impls: `SpeechToText`
      (one-shot — `audio-start` → chunked `audio-chunk` →
      `audio-stop` → `transcribe` → await `transcript`) and
      `StreamingStt` (when `info.asr.supports_transcript_streaming`
      is true; surface `transcript-chunk` as preview lane and the
      closing `transcript` as finalize lane). PCM conversion
      (Fono mono f32 → Wyoming int16 LE) at the codec boundary,
      saturating quantiser, sine-tone unit test.
- [ ] Task 2.3 — Wire into the STT factory at
      `crates/fono-stt/src/factory.rs`: backend id `"wyoming"`,
      config block `[stt.wyoming] uri = "tcp://host:port"
      model = "..." language = "..." token_ref = "..."`.
      Connection per call; `prewarm()` sends `describe` to validate
      reachability and cache supported models.
- [ ] Task 2.4 — Register in `fono-core::providers` so
      `fono use stt wyoming --uri tcp://...` works and `fono
      doctor` lists the row under Providers.
- [ ] Task 2.5 — In-process integration test: stand up a tokio
      task implementing the Wyoming server subset enough to
      validate a `transcribe` round-trip with a tiny PCM buffer;
      assert `Transcription.text` matches the canned response;
      cover both streaming and non-streaming paths.

### Slice 3 — Wyoming STT server (Fono serves Home Assistant et al.)

- [ ] Task 3.1 — Implement `fono_net::wyoming::server::WyomingServer`
      behind `wyoming-server`. `tokio::net::TcpListener`, one
      task per connection, bounded mpsc. Synthesises `info` from
      the active `WhisperLocal` model name + language list +
      `fono` crate version. Streaming path emits
      `transcript-start`/`transcript-chunk`/`transcript-stop`
      when the peer's `transcribe` event indicates streaming
      support.
- [ ] Task 3.2 — Daemon wiring: new `[server.wyoming]` block
      (`enabled`, `bind`, `port` default 10300, `auth_token_ref`,
      `models`); spawned at daemon start when `enabled`.
- [ ] Task 3.3 — Bind exposure simplification
      `[server.wyoming].bind` is the exposure control: default loopback for
      local-only serving, wildcard for all interfaces, or a specific interface
      address for one NIC. No separate public-bind override.
- [ ] Task 3.4 — Server-side connection-from-non-loopback rejection
      when `bind = "127.0.0.1"` (defence in depth).
- [ ] Task 3.5 — Integration test: in-process Wyoming client (the
      one from Task 2.5, reused) drives `WyomingServer` backed by
      the existing mock STT; round-trip `audio-start` → `transcript`
      and `audio-start` → `transcript-chunk` × N → `transcript-stop`.

### Slice 4 — mDNS autodiscovery (browser + advertiser)

- [ ] Task 4.1 — Add `mdns-sd = "0.11"` to workspace dependencies;
      `deny.toml` audit pass; document the license decision in
      ADR `0022-network-protocols.md` (drafted in Slice 5).
- [ ] Task 4.2 — Implement `fono_net::discovery::Browser` behind
      feature `discovery`. One tokio task per service type
      (`_wyoming._tcp.local.`, `_fono._tcp.local.`) maintaining a
      `RwLock<HashMap<ServiceId, DiscoveredPeer>>`.
      `DiscoveredPeer { kind: Wyoming|Fono, host, port, name,
      caps: Vec<String>, auth_required: bool, last_seen: Instant
      }`. Peers expire on TTL or a 60 s heartbeat miss.
- [ ] Task 4.3 — Implement `fono_net::discovery::Advertiser` behind
      feature `discovery`. Spawned by `WyomingServer` /
      `FonoServer` at startup; publishes the matching
      `_wyoming._tcp` / `_fono._tcp` record with TXT keys
      `proto`, `version`, `caps`, `auth`, `name`. Goodbye packet
      sent on graceful shutdown.
- [ ] Task 4.4 — Daemon hook: new orchestrator field
      `discovered: Arc<DiscoveryRegistry>` populated by the
      browser; a one-shot `prewarm` `describe` against each new
      Wyoming peer pre-caches its model list (so the tray submenu
      can show `Wyoming · kitchen-pc.local (whisper-small)` rather
      than just the host).
- [ ] Task 4.5 — IPC extension: new `Request::ListDiscovered` /
      `Response::Discovered(Vec<DiscoveredPeer>)` so the CLI
      (`fono discover`, see Task 4.7) and the tray
      (`DiscoveredProvider`) read from one source.
- [ ] Task 4.6 — Always-on discovery runtime: browser starts with the daemon,
      enabled servers advertise automatically, and disabled servers never
      advertise. `fono doctor` prints discovered peer count.
- [ ] Task 4.7 — CLI: `fono discover [--json]` lists current
      registry contents (host, port, kind, caps, auth, age).
- [ ] Task 4.8 — Integration test: spin up two in-process tokio
      tasks, one advertising `_fono._tcp` on a high port, one
      browsing; assert the second sees the first within 2 s and
      the goodbye packet evicts within 1 s of `Advertiser::shutdown`.

### Slice 5 — Fono-native protocol design + `FonoLlm` client

- [ ] Task 5.1 — Author ADR `docs/decisions/0022-network-protocols.md`
      capturing the two-protocol decision, mDNS choice, rejection
      rationale for gRPC / REST / extending-Wyoming-privately,
      and the upstream Wyoming README commit SHA we built against.
- [ ] Task 5.2 — Finalise the Fono-native event shapes:
      `Hello { client_version, capabilities, auth_token? }`,
      `HelloAck { server_version, capabilities, session_id }`,
      `Bye { reason }`,
      `CleanupRequest { id, raw_text, language?, app_context? }`,
      `CleanupResponse { id, cleaned_text, source_backend }`,
      `CleanupChunk { id, delta }` (reserved, not wired Slice 5),
      `HistoryAppend { id, raw, cleaned, app_context, ts }`,
      `Context { window_class?, window_title?, app_id? }`,
      `Error { code, message, retryable }`,
      `Ping { nonce }` / `Pong { nonce }`.
- [ ] Task 5.3 — Implement `fono_net::fono::client::FonoLlm` behind
      `fono-client`, implementing the existing `TextFormatter`
      trait. Connection per call (LAN RTT < 5 ms; pooling
      deferred until profiling shows it matters).
- [ ] Task 5.4 — Implement `fono_net::fono::client::FonoStt` behind
      `fono-client`, implementing `SpeechToText` + `StreamingStt`
      against the Fono-native protocol (so a single connection
      type handles both STT and LLM when both are on the same
      Fono server — distinct from selecting Wyoming for STT and
      Fono for LLM, which the user can also do).
- [ ] Task 5.5 — Wire both into the respective factories;
      backend ids `"fono-server"` for STT and `"fono-server"`
      for LLM; config blocks `[stt.fono]` / `[llm.fono]` with
      `uri`, `token_ref`, `model?`. Auth token negotiated in
      `Hello`; on mismatch, server replies `Error { code: AUTH }`
      and closes.
- [ ] Task 5.6 — `fono use stt fono-server --uri tcp://...` and
      `fono use llm fono-server --uri tcp://...` CLI; same code
      path as the existing `fono use` flow.
- [ ] Task 5.7 — Integration test: in-process Fono server hosts a
      mock LLM stub; client `FonoLlm.format(...)` round-trips
      through TCP loopback; same for `FonoStt.transcribe(...)`.

### Slice 6 — Fono-native server

- [ ] Task 6.1 — Implement `fono_net::fono::server::FonoServer`
      behind `fono-server`. Routes incoming `CleanupRequest` to
      the locally-configured `TextFormatter` (transparent: the
      server itself can be running local llama, cloud Anthropic,
      etc.); routes incoming `transcribe` to the locally-configured
      `SpeechToText`. Optional `mirror_history` writes
      `HistoryAppend` rows into the server's history DB; client's
      history remains source of truth.
- [ ] Task 6.2 — `[server.fono]` config block (`enabled`, `bind`,
      `port` default 10301, `auth_token_ref`, `mirror_history`).
- [ ] Task 6.3 — `fono serve [--wyoming] [--fono]` CLI flag —
      enables the matching server blocks at runtime without
      editing config; idempotent against a running daemon (sends
      `Request::Reload`). Default invocation `fono serve` enables
      both.
- [ ] Task 6.4 — RFC1918 + advertiser hooks reuse Slice 3/4
      helpers; advertiser starts when either `[server.wyoming]`
      or `[server.fono]` is enabled.
- [ ] Task 6.5 — Integration test: in-process Fono client +
      server end-to-end; assert mock `TextFormatter` on the server
      side actually runs and the client's
      `Request::Pipeline { … }` returns `cleaned_text` from it.

### Slice 7 — Tray + wizard + doctor surface

- [ ] Task 7.1 — Tray STT submenu extension at
      `crates/fono-tray/src/lib.rs`: existing "configured backends"
      list gets two new sections separated by disabled
      label-rows: **— Discovered on LAN —** (rows from
      `DiscoveredProvider`, ephemeral, rebuilt every 2 s tick) and
      **— Manually configured —** (today's list). Active backend
      gets the existing checkmark whether it came from manual
      config or an earlier discovery click.
- [ ] Task 7.2 — Same treatment for the tray LLM submenu.
- [ ] Task 7.3 — Click-discovered handler: writes the URI into
      the matching `[stt.<backend>].uri` (or
      `[llm.<backend>].uri`), prompts for an auth token via a
      desktop notification action when `auth_required = true`
      (mDNS TXT `auth=token`), then triggers `Request::Reload`.
      Token storage piggybacks on
      `crates/fono-core/src/secrets.rs`.
- [ ] Task 7.4 — Wizard step: after the existing local/cloud
      choice, if the discovery registry has at least one peer, the
      wizard offers a third path "Use a server I see on the
      network" with the discovered list as a multi-select; the
      Wyoming/Fono-native distinction is hidden behind the kind
      icon. Skipped silently if discovery is empty.
- [ ] Task 7.5 — `fono doctor` Network section: prints the
      current registry size and every enabled server's bind/port/peer-count.
- [ ] Task 7.6 — `fono hwprobe` extension: prints "this machine
      would make a good Fono server" / "this machine would make
      a good thin client" derived from existing
      `HardwareSnapshot` thresholds — informational only, never
      acts on it.

### Slice 8 — Polish, docs, release plumbing

- [ ] Task 8.1 — `docs/network.md`: full operator guide covering
      both directions, the auth-token flow, the discovery TXT
      schema, the LAN-only enforcement, and example
      `wyoming-faster-whisper` interop recipes.
- [ ] Task 8.2 — `docs/privacy.md`: document mDNS broadcast
      footprint and explain that server exposure is controlled by `bind`.
- [ ] Task 8.3 — Equivalence harness extension: new
      `fono-bench equivalence --stt wyoming --uri tcp://...` arm
      so a server-side regression is caught by the existing
      release-time gate when the operator runs it pre-tag.
- [ ] Task 8.4 — `CHANGELOG.md` `[Unreleased]` Added entries
      (Wyoming STT client, Wyoming STT server, Fono-native client
      + server, mDNS autodiscovery, tray discovered-peer
      submenus, `fono serve`, `fono discover`).
- [ ] Task 8.5 — `ROADMAP.md`: move "Whisper protocol" into
      Shipped on Slice 3 land; "Network inference" into Shipped
      on Slice 6 land. Both move under one release tag (likely
      `v0.4.0`).
- [ ] Task 8.6 — `docs/status.md` session entries at each slice
      boundary per the AGENTS.md rule.

## Verification Criteria

- Run `wyoming-faster-whisper` Docker image on machine A; on
  machine B, `fono` (no config edits) shows
  **— Discovered on LAN — Wyoming · A.local (whisper-large-v3)**
  in its tray STT submenu within 2 s of daemon start.
- Click that row on B → next dictation transcribes via A. Transcript
  matches `fono record --stt groq` on the same fixture within the
  existing `fono-bench equivalence` levenshtein tolerance.
- Run `fono serve` on machine A; on machine C running Home
  Assistant, the Wyoming integration auto-discovers
  `_wyoming._tcp` and connects without manual host:port entry.
- Run `fono serve` on machine A; on machine B, the tray LLM submenu
  shows **— Discovered on LAN — Fono server · A.local**. Click
  → next cleanup runs on A. Client-side CPU on B during a 10 s
  utterance does not exceed 10 % of one core.
- The discovery registry is repopulated fresh on daemon restart; no discovered
  peer state is persisted.
- `fono doctor` cleanly reports discovered peer count, server
  bind, RTT to last-used peer, and reachable server status.
- `cargo deny check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, and `tests/check.sh` (full matrix) all green
  including the new feature flags.
- Slim cloud-only build (`--no-default-features --features
  tray,cloud-all,wyoming-client,discovery`) compiles and
  binary-size delta is < 250 KB stripped vs current `v0.3.6`.
- mDNS goodbye packet evicts an advertised peer from the
  registry within 1 s of server shutdown (integration test).

## Potential Risks and Mitigations

1. **mDNS broadcast traffic on hostile or metered networks.**
   Multicast on a VPN or LTE-tethered link is wasteful and
   sometimes blocked. Mitigation: discovery only advertises enabled servers;
   disabled servers remain silent, and `fono doctor` flags when multicast
   appears unavailable.
2. **mDNS service-name collision.** `_wyoming._tcp.local.` is a
   namespace shared with anyone else publishing it. Mitigation:
   the TXT `proto` key and the connection-arm allow-list
   reject mismatched peers at handshake time.
3. **Auth token leakage on first connection.** Token is sent in
   the `Hello` event prologue. Mitigation: scoped per server,
   rotatable via `fono keys remove fono-server && fono keys
   add fono-server`; never logged; future TLS slice (out of
   scope for v1) moves to mutual auth.
4. **Discovered-peer name spoofing.** A malicious LAN peer can
   advertise `_fono._tcp.local.` with `name=studio.local`.
   Mitigation: clicking a discovered row never auto-applies an
   auth token; the token prompt is mandatory on first selection
   when `auth=token` is advertised; `fono doctor` shows the
   resolved IP address alongside the friendly name.
5. **Wyoming streaming compatibility drift.**
   `transcript-chunk` is recent; not every server supports it.
   Mitigation: detect via `info.asr.supports_transcript_streaming`
   and fall back to single-`transcript`. Both paths integration-
   tested.
6. **PCM format mismatch.** Wyoming int16 LE vs Fono mono f32.
   Mitigation: convert at codec boundary using `rubato` for
   resample + saturating int16 quantiser; unit-tested with
   sine-tone fixtures.
7. **Two protocols, one codec — accidental cross-leak.**
   Mitigation: connection-arm types each carry an event-tag
   allow-list enforced at parse time (Slice 1 Task 1.5).
8. **`mdns-sd` crate maintenance risk.** It's a single-maintainer
   crate. Mitigation: surface area we depend on is tiny
   (`ServiceDaemon::new`, `browse`, `register`); a fork+vendor
   contingency is < 200 LoC of work and documented in the ADR.
9. **Public-bind footgun amplified by mDNS.** A user setting
   `bind = "0.0.0.0"` on coffee-shop Wi-Fi exposes the server on every
   interface and advertises it on the local subnet. Mitigation: keep the
   default loopback bind for local-only serving; operator-facing docs make
   `bind` the only exposure control.

## Alternative Approaches

1. **Avahi/Bonjour FFI instead of `mdns-sd`.** Trade-off:
   leverages the system's existing daemon (no duplicate listener);
   downside is a hard system dep on `libavahi-client` (not always
   present, especially on minimal NimbleX hosts), violates the
   single-binary stance, and the Windows/macOS path needs Bonjour
   FFI on top. Rejected.
2. **DHT-based discovery (e.g. Kademlia) for cross-LAN reach.**
   Trade-off: works without multicast; downside is operational
   complexity and a privacy footprint we explicitly want to
   avoid in v1. Deferred to a hypothetical "WAN inference"
   roadmap entry.
3. **HTTP/SSE instead of JSONL-over-TCP.** Trade-off: trivial
   debugging with `curl`; downside is one-way streaming, awkward
   audio uploads, and we lose Wyoming codec reuse. Rejected.
4. **gRPC for Fono-native protocol.** Trade-off: codegen, strong
   typing, streaming for free; downside is `tonic` adds ~3 MB to
   the binary and a `protoc` build dep. Reconsidered in v2 only
   if the Fono-native event surface exceeds ~30 types. Rejected
   for v1.
5. **Defer Fono-native protocol; ship Wyoming + discovery only.**
   Trade-off: smallest scope; downside is LLM cleanup still has
   to run client-side, and the user's "thin client on a 10-year-old
   laptop" experience is half-delivered. The plan keeps Slices
   1–4 + 7 (tray) shippable as a "v0.4 Wyoming + discovery"
   release if Slices 5–6 slip; Slices 5–6 then become "v0.5
   Network inference."
