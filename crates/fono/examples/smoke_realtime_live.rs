// SPDX-License-Identifier: GPL-3.0-only
//! Live full-duplex realtime smoke test (Slice 0 of the realtime
//! live-conversation plan).
//!
//! This is a standalone, hands-on harness for the single biggest
//! unknown in the live-mode design: does a *full-duplex* Gemini Live
//! session actually behave — continuous mic in, continuous reply audio
//! out, server-side VAD owning the turn boundaries, and acoustic
//! barge-in when you talk over the model. It deliberately does NOT
//! touch the daemon, the hotkey FSM, the overlay, or PipeWire AEC; it
//! wires the realtime client straight to the mic and the speakers so
//! the behaviour can be judged in isolation before any of that
//! integration work lands.
//!
//! ## Running it
//!
//! Use **headphones** — without echo cancellation (which this harness
//! does not set up) speaker output will feed back into the mic and the
//! model will interrupt itself. Run from the workspace root so
//! `tests/secrets.toml` is found:
//!
//! ```sh
//! cargo run --release --example smoke_realtime_live -p fono
//! ```
//!
//! It opens a persistent full-duplex session, streams your mic
//! continuously, plays the model's reply, prints both transcripts, and
//! loops across many turns in one session (the whole point — turns
//! 2..N reuse the open socket). Talk over the model to verify
//! barge-in. Press Ctrl-C to leave.
//!
//! The `GEMINI_API_KEY` is read from `tests/secrets.toml`, then
//! `~/.config/fono/secrets.toml`, then the process environment.
//!
//! Exit code 0 on a clean Ctrl-C exit; non-zero if the session could
//! not be built/opened.

#[cfg(not(feature = "realtime"))]
fn main() {
    eprintln!(
        "smoke_realtime_live requires the `realtime` feature (default-on).\n\
         Re-run without disabling default features."
    );
    std::process::exit(2);
}

#[cfg(feature = "realtime")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    realtime_live::run().await
}

#[cfg(feature = "realtime")]
mod realtime_live {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use anyhow::{anyhow, Result};
    use fono_assistant::{
        AssistantContext, AssistantHandle, ConversationHistory, RealtimeEvent, RealtimeMode,
        RealtimeSession,
    };
    use fono_audio::{AudioCapture, AudioPlayback, CaptureConfig};
    use fono_core::config::{Assistant as AssistantCfg, AssistantBackend, AssistantCloud};
    use fono_core::{provider_catalog, Paths, Secrets};
    use futures::StreamExt;

    /// Load secrets from the workspace fixture, the runtime config dir,
    /// or fall back to the process environment — mirrors
    /// `smoke_assistant`'s discovery so the same key works everywhere.
    fn load_secrets() -> Result<Secrets> {
        let workspace = std::path::PathBuf::from("tests/secrets.toml");
        let path = if workspace.exists() {
            Some(workspace)
        } else {
            let p = Paths::resolve()?.secrets_file();
            p.exists().then_some(p)
        };
        if let Some(p) = path.as_ref() {
            println!("secrets file: {}", p.display());
        } else {
            println!("secrets file: (none — reading from env)");
        }
        Ok(path.as_ref().map(|p| Secrets::load(p).unwrap_or_default()).unwrap_or_default())
    }

    /// Build the Gemini realtime handle by selecting the catalogue's
    /// realtime profile model — the same path the daemon's
    /// `build_assistant_handle` takes.
    fn build_realtime(
        secrets: &Secrets,
    ) -> Result<std::sync::Arc<dyn fono_assistant::RealtimeAssistant>> {
        let profile = provider_catalog::find("gemini")
            .and_then(|e| e.assistant.as_ref())
            .and_then(|a| a.realtime)
            .ok_or_else(|| anyhow!("no Gemini realtime profile in the provider catalogue"))?;
        println!(
            "realtime model: {} ({} Hz in / {} Hz out)",
            profile.model, profile.input_sample_rate, profile.output_sample_rate
        );

        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".to_string(),
                api_key_ref: "GEMINI_API_KEY".to_string(),
                model: profile.model.to_string(),
            }),
            ..AssistantCfg::default()
        };
        match fono_assistant::build_assistant_handle(&cfg, secrets, std::path::Path::new("."))? {
            Some(AssistantHandle::Realtime(rt)) => Ok(rt),
            Some(AssistantHandle::Staged(_)) => {
                Err(anyhow!("factory returned a staged assistant; realtime selection failed"))
            }
            None => Err(anyhow!(
                "assistant disabled or GEMINI_API_KEY missing — run `fono keys add GEMINI_API_KEY`"
            )),
        }
    }

    pub async fn run() -> Result<()> {
        // `--mute-while-speaking` is a diagnostic / no-AEC fallback: it
        // stops forwarding mic audio while the model is producing reply
        // audio. Without echo cancellation the mic otherwise picks up the
        // model's own playback, the input transcriber turns that into
        // bogus "user" text (foreign-language fragments, chopped echoes),
        // and server VAD barges in on every reply. Gating the mic proves
        // the echo hypothesis and yields working multi-turn conversation
        // at the cost of barge-in (which needs real AEC — plan Part E).
        let mute_while_speaking = std::env::args().any(|a| a == "--mute-while-speaking");

        println!("Fono realtime LIVE (full-duplex) smoke test\n");
        if mute_while_speaking {
            println!("mode: mute-while-speaking (barge-in OFF — no-AEC fallback)\n");
        } else {
            println!(
                "mode: full barge-in. Use headphones AND make sure playback is routed to them;\n\
                 any model audio reaching the mic will self-interrupt. Try --mute-while-speaking\n\
                 to confirm echo is the cause.\n"
            );
        }

        let secrets = load_secrets()?;
        let realtime = build_realtime(&secrets)?;
        let input_rate = realtime.native_input_rate();

        // Seed an empty conversation; the model transcribes the user
        // turn itself in full-duplex mode.
        let ctx = AssistantContext {
            system_prompt: "You are Fono, a concise spoken assistant. Keep replies short."
                .to_string(),
            language: None,
            history: ConversationHistory::default().snapshot(),
            active_window_context: None,
            screen_capture: None,
            prefer_vision: false,
            max_new_tokens: None,
            allow_brain_capture: false,
        };

        println!("opening full-duplex session…");
        let RealtimeSession { audio_in, mut events } =
            realtime.open_session(&ctx, RealtimeMode::FullDuplex).await?;
        println!("session open — start talking. Ctrl-C to leave.\n");

        // Continuous mic capture wired straight into the session input
        // sink. The capture backend resamples to the model's native
        // input rate for us. The forwarder runs on the capture thread,
        // so it must stay cheap: a non-blocking try_send that drops on
        // a full channel rather than blocking the audio thread.
        let capture =
            AudioCapture::new(CaptureConfig { target_sample_rate: input_rate, source: None });
        let mic_sink = audio_in.clone();
        // Shared with the event loop: true while the model is producing
        // reply audio. When `--mute-while-speaking` is set, the forwarder
        // drops mic frames during that window so the model cannot hear
        // (and re-transcribe) itself.
        let speaking = Arc::new(AtomicBool::new(false));
        let speaking_fwd = Arc::clone(&speaking);
        let _cap = capture.start_with_forwarder(move |pcm: &[f32]| {
            if mute_while_speaking && speaking_fwd.load(Ordering::Relaxed) {
                return;
            }
            let _ = mic_sink.try_send(pcm.to_vec());
        })?;

        // Playback handle for the reply audio. We stream chunks gaplessly
        // within a turn and tear the stream down on Done/Interrupted.
        let playback = AudioPlayback::new(None)?;

        let mut streaming = false;
        let mut turn = 0_u32;
        let mut barge_ins = 0_u32;
        let mut turn_started: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("\nctrl-c — closing session");
                    break;
                }
                ev = events.next() => {
                    match ev {
                        None => { println!("\nsession closed by server"); break; }
                        Some(Err(e)) => { eprintln!("\nevent error: {e:#}"); break; }
                        Some(Ok(event)) => handle_event(
                            event,
                            &playback,
                            &speaking,
                            &mut streaming,
                            &mut turn,
                            &mut barge_ins,
                            &mut turn_started,
                        )?,
                    }
                }
            }
        }

        // Dropping `audio_in` (and the capture handle) closes the input;
        // dropping the session struct's `events` closes the socket.
        drop(audio_in);
        playback.stop();
        println!("done — {turn} turn(s), {barge_ins} barge-in(s) in one session");
        Ok(())
    }

    /// Apply one reply event to the playback stream and the on-screen
    /// transcript, tracking turn/barge-in counters and the `speaking`
    /// flag the mic forwarder consults.
    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        event: RealtimeEvent,
        playback: &AudioPlayback,
        speaking: &Arc<AtomicBool>,
        streaming: &mut bool,
        turn: &mut u32,
        barge_ins: &mut u32,
        turn_started: &mut Option<Instant>,
    ) -> Result<()> {
        use std::io::Write;
        match event {
            RealtimeEvent::Audio { pcm, sample_rate } => {
                if !*streaming {
                    playback.begin_stream()?;
                    *streaming = true;
                    speaking.store(true, Ordering::Relaxed);
                    if let Some(t) = turn_started.take() {
                        println!("  (ttfa {} ms)", t.elapsed().as_millis());
                    }
                }
                playback.push_stream(pcm, sample_rate)?;
            }
            RealtimeEvent::AssistantTextDelta(s) => {
                print!("{s}");
                let _ = std::io::stdout().flush();
            }
            RealtimeEvent::UserTextFinal(s) => {
                *turn += 1;
                *turn_started = Some(Instant::now());
                println!("\n[you] {s}");
            }
            RealtimeEvent::Interrupted => {
                playback.stop();
                *streaming = false;
                speaking.store(false, Ordering::Relaxed);
                *barge_ins += 1;
                println!("  [barge-in: reply discarded]");
            }
            RealtimeEvent::Done => {
                if *streaming {
                    playback.end_stream()?;
                    *streaming = false;
                }
                speaking.store(false, Ordering::Relaxed);
                println!("  [turn done]");
            }
            RealtimeEvent::EndConversation => {
                println!("  [model ended the conversation]");
            }
        }
        Ok(())
    }
}
