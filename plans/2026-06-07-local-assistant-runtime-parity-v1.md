# Local assistant runtime parity resume plan

Date: 2026-06-07

## Objective

Bring Fono's embedded local assistant path (`llama-cpp-rs` / in-process `llama.cpp`) as close as possible to the local server benchmark path before doing higher-level product optimizations.

The comparison target is the fast local OpenAI-compatible server benchmark for Gemma E2B CPU, especially the `ctx2048 slots1` results that were around sub-second median latency. The embedded path has already improved substantially after streaming and stop-marker fixes, but it still trails the server path.

## Current state

### Already implemented in this session

- Embedded local assistant crash fix via the patched `llama-cpp-rs` fork is in place in `Cargo.lock`.
- Assistant tracing exists and writes Chrome Trace / Perfetto JSON when `FONO_ASSISTANT_TRACE` is set.
- Prompt text is now captured by default when assistant tracing is enabled, unless `FONO_ASSISTANT_TRACE_PROMPT=0` is set.
- Embedded local assistant now streams deltas instead of returning one final chunk.
- Embedded stop-marker handling was improved for markers such as `<end_of_turn>` and `<start_of_turn>`.
- `fono-bench assistant-factual` can benchmark embedded `llama.cpp` with `--provider llama-cpp`.
- Embedded benchmark knobs were added for `--batch-size` and `--ubatch-size`.
- Embedded local assistant now defaults to CPU-friendlier batch settings:
  - `batch = min(2048, context_size)`
  - `ubatch = min(512, batch)`

### Main findings so far

- The old fast benchmark was server-backed, not embedded.
- After streaming, embedded factual CPU benchmark latency is roughly in the 1.0-1.6s range depending on profile/config, not the earlier 18s anomaly.
- The local server benchmark is still faster, roughly around 0.55s median in the previous artifacts.
- Live F8 with local TTS exposed severe CPU contention: local TTS ONNX inference starved LLM decode.
- Switching to cloud TTS made LLM token cadence healthy again, confirming that local TTS contention is a separate issue from embedded-vs-server runtime parity.
- History/prompt growth matters, but the immediate runtime-parity work should compare exact same prompts through embedded and server backends.

## Big-picture plan

1. Establish exact-prompt parity tests.
   - Extract the exact prompt from a Fono assistant trace.
   - Replay that prompt through embedded `llama.cpp`.
   - Replay the same prompt through the local OpenAI-compatible server.
   - Compare total latency, TTFB, delta count, and generated output.

2. Measure server-side internals where possible.
   - If the local server exposes Ollama-native stats, capture prompt eval count/duration and eval count/duration.
   - Compare embedded prefill tokens/sec and decode tokens/sec against server prompt/eval tokens/sec.

3. Close the remaining embedded/server runtime gap.
   - Continue batch/ubatch/thread sweeps using exact prompts.
   - Compare CPU build flags / dispatch features between embedded `llama.cpp` and the server runtime.
   - Only then investigate slot/context/KV reuse, because context creation itself was not the main cost in traces.

4. After LLM runtime parity, fix local TTS contention.
   - Limit local TTS ONNX threads.
   - Avoid oversubscribing CPU when local LLM and local TTS overlap.
   - Consider a CPU semaphore or throttling policy for concurrent local inference engines.

## Completed this session

The resume step is complete. `fono-bench assistant-replay` now supports exact raw-prompt replay through both embedded `llama-cpp` and HTTP/OpenAI-compatible chat providers, and `extract-trace-prompt` extracts captured prompts from assistant traces with prompt length and SHA-256 reporting.

The HTTP replay path sends one user message containing the raw prompt, keeps streaming enabled, and records latency, TTFB, delta count, output length, output text, and prompt SHA-256 in the replay report. Embedded replay continues to use `fono_assistant::llama_local::LlamaLocalAssistant::reply_raw_prompt_stream` so both paths measure streaming behavior.

Verification run on 2026-06-07:

```bash
cargo fmt --all -- --check
cargo check -p fono-bench --features llama-local
cargo test -p fono-bench --features llama-local --lib --bins --tests
cargo clippy -p fono-bench --features llama-local --all-targets --no-deps -- -D warnings
cargo clippy -p fono-assistant --features llama-local --all-targets -- -D warnings
```

## Current step to resume

The current unfinished step is:

> Completed: Extend `fono-bench assistant-replay` so exact prompt replay supports both embedded `llama.cpp` and HTTP/OpenAI-compatible assistant providers.

This is complete in `crates/fono-bench/src/bin/fono-bench.rs`.

The desired user-facing workflow is:

```bash
# Extract a prompt from a trace into a plain text file.
cargo run -p fono-bench -- extract-trace-prompt \
  --trace /tmp/fono-traces/assistant-XXXX.json \
  --out /tmp/f8-prompt.txt

# Replay that exact prompt through embedded llama.cpp.
cargo run -p fono-bench --features llama-local -- assistant-replay \
  --provider llama-cpp \
  --model-path /root/.cache/fono/models/polish/gemma-4-e2b.gguf \
  --prompt-file /tmp/f8-prompt.txt \
  --ctx-size 8192 \
  --threads 8 \
  --batch-size 2048 \
  --ubatch-size 512 \
  --iterations 3

# Replay the same prompt through the local OpenAI-compatible server.
cargo run -p fono-bench -- assistant-replay \
  --provider ollama \
  --endpoint http://127.0.0.1:18131/v1/chat/completions \
  --model gemma-4-E2B-q4_0-cpu-ctx2048-slots1 \
  --prompt-file /tmp/f8-prompt.txt \
  --iterations 3
```

## Implementation notes for the resume step

### Existing embedded replay path

`assistant-replay` currently exists for embedded raw-prompt replay. It should be generalized rather than duplicated.

The embedded path should continue using the local assistant raw prompt helper in `fono_assistant::llama_local`.

### HTTP/server replay path

For HTTP/OpenAI-compatible replay, do not use the normal assistant history/prompt builder. The purpose is byte-for-byte prompt replay.

Send a single message containing the raw prompt, preferably as:

```json
[{ "role": "user", "content": "<raw prompt file contents>" }]
```

Keep the request streaming so the benchmark records true TTFB and delta count.

The HTTP replay implementation can be local to `fono-bench` instead of changing the production assistant provider API.

### Trace prompt extraction helper

Add a small `extract-trace-prompt` command to `fono-bench` that:

- reads a Chrome Trace JSON file,
- finds the first event whose `args.prompt` is a string,
- writes that prompt to `--out`, or stdout if no output path is given,
- reports prompt length and SHA-256.

This keeps private prompt capture explicit at the command level while trace capture remains convenient.

## Validation to run after resuming

Run at least:

```bash
cargo fmt --all -- --check
cargo check -p fono-bench --features llama-local
cargo test -p fono-bench --features llama-local --lib --bins --tests
cargo clippy -p fono-bench --features llama-local --all-targets --no-deps -- -D warnings
```

If touching `fono-assistant` again, also run:

```bash
cargo clippy -p fono-assistant --features llama-local --all-targets -- -D warnings
```

## Important caution

Keep local TTS out of LLM runtime parity measurements. Use no TTS or cloud TTS while comparing embedded and server LLM performance. Local TTS contention is real, but it is a separate optimization track.
