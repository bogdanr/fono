# Resumable implementation plan: Gemma E2B local LLM default

## Status: Completed

## Goal

Simplify Fono’s local LLM strategy:

- Use **Gemma 4 E2B Q4_0** as the local LLM model for both:
  - cleanup/polish
  - assistant
- Preserve the existing **hardware tier structure** for future model choices.
- For now, select Gemma E2B across all tiers.
- Use hardware tiering only to decide whether **local cleanup** is enabled/recommended.
- Always allow/use Gemma E2B for the **local assistant**, including older CPU-only machines.
- Fix the **Qwen/ChatML hardcoded prompt/template path** that hurt Gemma cleanup performance.

---

## 1. Product decision to encode

### 1.1 Local model choice

Use one default local LLM:

```text
gemma-4-E2B_q4_0-it.gguf
```

Model source:

```text
google/gemma-4-E2B-it-qat-q4_0-gguf
```

License status:

- Apache-2.0 per model repo/model card.
- Project policy was already updated to allow OSI-approved Gemma artifacts as defaults.

### 1.2 Runtime defaults

Default local text LLM runtime:

```text
backend: llama.cpp server / OpenAI-compatible endpoint
model: gemma-4-E2B_q4_0-it.gguf
ctx: 2048
slots: 1
thinking/reasoning: disabled
jinja/native template: enabled
vision/mmproj: disabled by default
```

Optional local vision mode:

```text
model: gemma-4-E2B_q4_0-it.gguf
mmproj: gemma-4-E2B-it-mmproj.gguf
```

Do **not** load mmproj by default.

### 1.3 Role policy

Assistant:

```text
Always local Gemma E2B when local assistant is selected.
```

Cleanup:

```text
Use local Gemma E2B only when hardware tier says local cleanup is acceptable.
Otherwise use cloud cleanup or disable cleanup by default.
```

No Qwen fallback in normal/default logic.

---

## 2. Benchmark evidence behind the plan

### 2.1 Gemma E2B on main machine

Gemma E2B Vulkan, optimized server path:

Assistant factual:

```text
~15/16 pass
p50 ~0.30s
```

Cleanup:

```text
6/9 pass
p50 ~0.8–0.9s
```

Gemma E2B CPU server path:

Assistant:

```text
16/16 in one ctx1024 CPU-server sanity run
~0.58s p50 on the main machine
```

Cleanup:

```text
6/9
~2.1s p50 on the main machine
```

### 2.2 Old ThinkPad i7-7500U

Gemma E2B via optimized Ollama direct API:

Assistant:

```text
14/16 pass
p50 ~2.1s
p95 ~3.7s
```

Cleanup:

```text
4/9 pass
p50 ~4.6s
p95 ~5.3s
```

Conclusion:

- Assistant is acceptable even on old CPU-only hardware.
- Cleanup is not acceptable on old CPU-only hardware.

### 2.3 Qwen conclusion

Qwen models should not be part of the default path:

- Qwen 0.8B: too weak.
- Qwen 2B: no practical edge over Gemma E2B.
- Qwen 4B: only marginal factual edge on a small fixture set; heavier and slower.
- Qwen-specific templates hurt Gemma cleanup if reused.

Keep Qwen only as manual/experimental if desired, not as a default tier choice.

---

## 3. Files to inspect/update

### 3.1 Local defaults and config

Inspect/update:

```text
crates/fono-core/src/config.rs
```

Likely items:

- `DEFAULT_POLISH_LOCAL_MODEL`
- local polish default config
- assistant local default config
- comments saying assistant needs “3B-class” or references older Qwen assumptions

Expected changes:

```text
DEFAULT_POLISH_LOCAL_MODEL = "gemma-4-e2b"
assistant local default = same Gemma E2B identifier
ctx default = 2048
local assistant token budget = already tightened to 256; keep
local cleanup budget = already input-size based; keep
```

Keep tier structs/fields intact.

---

### 3.2 Local model registry

Inspect/update:

```text
crates/fono-polish/src/registry.rs
```

Current registry likely contains:

```text
qwen3.5-0.8b
qwen3.5-2b
```

Expected changes:

Add Gemma E2B entry:

```text
id: "gemma-4-e2b"
display_name: "Gemma 4 E2B Instruct Q4_0"
repo: "google/gemma-4-E2B-it-qat-q4_0-gguf"
filename: "gemma-4-E2B_q4_0-it.gguf"
license: "Apache-2.0"
approx_size: ~3.35 GiB
languages/multilingual: true
default_eligible: true
```

Optional separate vision/projector metadata:

```text
mmproj filename: "gemma-4-E2B-it-mmproj.gguf"
mmproj size: ~942 MiB on disk
runtime estimate: +~1.2 GiB
```

Do not download/load mmproj by default.

Decide whether to keep Qwen entries:

- It is okay to keep as experimental/manual.
- But tier/default selection should choose Gemma.

---

### 3.3 Wizard tier selection

Inspect/update:

```text
crates/fono/src/wizard.rs
```

Known previous behavior:

- Wizard preserved hardware tier logic.
- It selected `qwen3.5-0.8b` for normal/default local cleanup.
- It selected `qwen3.5-2b` for high-end CPU / LLM acceleration.

Expected change:

Keep the tier logic, but make all local LLM tiers select Gemma E2B.

Conceptually:

```rust
fn choose_local_llm_model(_tier: HardwareTier) -> &'static str {
    "gemma-4-e2b"
}
```

Cleanup enablement should remain tier-dependent:

```text
fast/Vulkan/high-end tier:
  local cleanup enabled/recommended

old CPU-only tier:
  local cleanup disabled/cloud/no-cleanup by default
```

Assistant should not be disabled just because cleanup is disabled:

```text
assistant local Gemma E2B remains allowed/enabled
```

Update wizard copy:

- Remove “small Qwen for default / Qwen 2B for high-end”.
- Explain:
  - Gemma E2B is the local assistant model.
  - Local cleanup requires faster hardware.
  - Old CPU-only machines can use local assistant but cloud/no cleanup is recommended for cleanup.

---

### 3.4 Download/model management

Inspect/update:

```text
crates/fono/src/models.rs
```

Expected changes:

- Ensure Gemma E2B can be downloaded/located.
- Ensure model filename/repo matches registry.
- Ensure model destination is consistent with local LLM use:
  - probably `/root/.cache/fono/models/llm/`
  - or whatever Fono uses for local polish models
- Avoid downloading mmproj unless local vision is explicitly enabled.

---

## 4. Critical template fix

### 4.1 Problem

Gemma performance was hurt by using a Qwen/ChatML-style in-process cleanup template.

Inspect:

```text
crates/fono-polish/src/llama_local.rs
```

Problem area:

- Hardcoded ChatML/Qwen prompt around existing lines previously inspected near:
  - `llama_local.rs:223-254`
- Qwen thinking-specific handling around:
  - `llama_local.rs:233-239`
- Qwen/SmolLM stop tokens like `<|im_end|>` around:
  - `llama_local.rs:191-199`

Bad benchmark symptom:

```text
Gemma CPU/in-process cleanup produced <|im_end|> residue and repetition.
```

Good benchmark symptom:

```text
Gemma server/OpenAI-compatible path with --jinja/native template worked much better.
```

### 4.2 Required fix

Do **not** use the current hardcoded in-process `llama_local` ChatML path for Gemma.

Implement one of these:

#### Preferred short-term fix

Route Gemma local cleanup through the OpenAI-compatible llama.cpp server path.

That path already supports:

- model-native Jinja template via server
- `think: false`
- `chat_template_kwargs.enable_thinking: false`
- compact cleanup token budget

Relevant files:

```text
crates/fono-polish/src/openai_compat.rs
crates/fono-assistant/src/openai_compat_chat.rs
```

#### Long-term fix

Update in-process llama.cpp backend to use model-native chat templates instead of hardcoded ChatML.

Possible approaches:

- expose llama.cpp chat template application if bindings support it;
- detect model family and use correct template;
- or deprecate in-process local LLM cleanup in favor of managed llama.cpp server.

### 4.3 Guardrail

If selected model is Gemma:

```text
must not use hardcoded Qwen/ChatML llama_local prompt
```

Add a test if possible:

- Given Gemma registry/default model, factory chooses OpenAI/server backend, not in-process `llama_local`.
- Or unit-test that Gemma is not passed through Qwen template builder.

---

## 5. Shared local server architecture

Implement or plan toward:

```text
one warm local LLM server
one loaded Gemma E2B model
cleanup and assistant both call it through OpenAI-compatible API
```

Benefits:

- one memory cost
- correct Gemma template
- prompt cache
- consistent no-thinking behavior
- avoids loading separate cleanup and assistant models

Runtime settings:

```text
ctx = 2048
slots = 1
--jinja
--reasoning off
Vulkan when available
text-only by default
```

If true concurrency is needed later:

```text
ctx = 4096
slots = 2
```

But default should be one slot.

---

## 6. Hardware tier behavior

Preserve existing tier structure.

Do not tear out current hardware tiering.

Instead, map tiers like this:

```text
All tiers:
  assistant_model = gemma-4-e2b

Fast/Vulkan/high-end tiers:
  cleanup_model = gemma-4-e2b
  cleanup_local_enabled = true

Old CPU-only / low-end tiers:
  cleanup_local_enabled = false
  cleanup = cloud or disabled by default
```

Potential signals already available in code:

```text
crates/fono-core/src/hwcheck.rs
```

Look for:

- Vulkan availability
- CPU generation
- core/thread count
- RAM
- AVX/AVX2/AVX-VNNI flags
- existing “high_end” / “llm_acceleration” classification

Threshold idea:

```text
if Vulkan acceleration available:
    cleanup local enabled
else if CPU is modern/high-end enough:
    cleanup local optional/enabled
else:
    cleanup local disabled by default
```

Assistant does not need these strict thresholds.

---

## 7. Vision/screenshot support

Do not load projector by default.

Optional local vision mode:

```text
gemma-4-E2B_q4_0-it.gguf
gemma-4-E2B-it-mmproj.gguf
```

Measured:

```text
mmproj file: ~942 MiB
estimated runtime memory: +~1.2 GiB
normal text slowdown: none meaningful in quick benchmark
```

But screenshots are not needed for default assistant/cleanup.

If implemented later:

- expose as separate opt-in local vision toggle;
- use `--image-max-tokens` to cap screenshot cost;
- benchmark actual screenshot latency/quality separately.

---

## 8. Tests to add/update

### 8.1 Registry tests

Add/update tests for:

- Gemma E2B exists in registry.
- Gemma E2B license is Apache-2.0.
- Gemma E2B is default-eligible.
- Qwen entries, if kept, are not selected as tier defaults.

### 8.2 Config tests

Add/update tests for:

- default local polish model is Gemma E2B.
- assistant local default model is Gemma E2B.
- local context default is 2048 if config exposes it.
- local assistant/cleanup still disable thinking.

### 8.3 Wizard tests

Add/update tests for:

- all local LLM tiers select Gemma E2B.
- low-end tier disables/skips local cleanup but keeps local assistant.
- Vulkan/high-end tier enables local cleanup.
- wizard text no longer claims Qwen is the local default.

### 8.4 Template/backend tests

Add/update tests for:

- Gemma cleanup does not use Qwen/ChatML `llama_local` prompt.
- Gemma cleanup goes through OpenAI-compatible/native-template path.
- Qwen-specific `<think></think>` seeding is not applied to Gemma.
- Hardcoded `<|im_end|>` stop-token behavior is not assumed for Gemma.

---

## 9. Validation commands

After implementation:

```text
cargo fmt --all -- --check
cargo test -p fono-core -p fono-polish -p fono --lib
cargo clippy -p fono-core -p fono-polish -p fono --all-targets -- -D warnings
```

If touching assistant request path:

```text
cargo test -p fono-assistant openai_compat --lib
cargo clippy -p fono-assistant --all-targets -- -D warnings
```

Before any commit:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --tests --lib
```

Commit must be signed off:

```text
git commit -s
```

Do not push unless explicitly instructed.

---

## 10. Suggested implementation order

### Step 1: Registry

- [x] Add Gemma E2B model entry.
- [x] Keep Qwen entries as manual/experimental if desired.
- [x] Add registry tests.

### Step 2: Config defaults

- [x] Set default local polish/assistant model to Gemma E2B.
- [x] Ensure ctx default is 2048 where applicable.
- [x] Keep local token budgets optimized.

### Step 3: Wizard tier mapping

- [x] Preserve tier functions/structure.
- [x] Map every local LLM tier to Gemma E2B.
- [x] Gate cleanup enablement by hardware tier.
- [x] Keep assistant local enabled.

### Step 4: Template/backend routing

- [x] Prevent Gemma from using `llama_local` hardcoded Qwen/ChatML prompt.
- [x] Prefer OpenAI-compatible server/native-template path for Gemma.
- [x] Add tests or guardrails.

### Step 5: Model download/management

- [x] Ensure Gemma E2B downloads from the correct HF repo/filename.
- [x] Do not auto-download mmproj.
- [x] Optional: add mmproj metadata for future local vision mode.

### Step 6: Tests and validation

- [x] Run targeted tests.
- [x] Run fmt/clippy.
- [x] Fix any fallout.

---

## 11. Final intended outcome

After implementation:

```text
Fono local LLM model: Gemma 4 E2B Q4_0
Assistant: local Gemma E2B everywhere local assistant is enabled
Cleanup: local Gemma E2B only on capable hardware tiers
Old CPU-only: local assistant okay, cleanup cloud/disabled
Vision: opt-in mmproj, off by default
Qwen: no longer normal/default local model
Templates: Gemma never uses Qwen/ChatML hardcoded cleanup template
```

This is the simplest product model while preserving the tier framework for future model discoveries.
