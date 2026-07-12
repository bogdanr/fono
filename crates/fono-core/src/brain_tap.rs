// SPDX-License-Identifier: GPL-3.0-only
//! Brain tap — sparse, near-zero-cost capture of per-token forward-pass
//! signals from the embedded llama.cpp backends, feeding the "Glass
//! Cortex" overlay visualization (see
//! `plans/2026-07-05-brain-visualization-v1.md`).
//!
//! ## How it works
//!
//! llama.cpp's scheduler exposes a per-graph-node eval callback
//! (`cb_eval` on the context params — the same hook the upstream
//! `imatrix` tool uses). The callback is invoked twice per node: once
//! with `ask = true` ("do you want to observe this node?") and, when we
//! answered yes, once with `ask = false` after the node's data is
//! available.
//!
//! The tap is **demand-armed**: the decode loop arms it only for the
//! specific tokens the visualization needs a keyframe for (a handful
//! per second of *TTS playback*, not per token generated). While
//! disarmed the callback answers "not interested" to every `ask` in a
//! single relaxed atomic load — llama.cpp then never materialises any
//! data for us, so the steady-state cost is one predictable branch per
//! graph node. The `< 1 %` decode-overhead budget is enforced by
//! [`SampleGovernor`], which measures sampled-vs-unsampled token times
//! and widens the sampling interval when a sampled token proves
//! expensive (e.g. GPU→CPU syncs on a discrete-VRAM backend).
//!
//! ## What is captured per keyframe
//!
//! - `l_out-<layer>` — the layer's output hidden state; reduced to an
//!   L2 norm **inside the callback** (the full activation never leaves
//!   the capture path), one `f32` per layer.
//! - `ffn_moe_topk-<layer>` / `ffn_moe_weights-<layer>` — the routed
//!   expert ids and their routing weights (MoE models only; absent on
//!   dense models). These tensors are tiny (top-k entries per token).
//!
//! Tensor names are stable graph-callback names assigned by
//! `llm_graph_context::cb()` in llama.cpp (`"name-<il>"` format); the
//! matching is name-pattern based, never index based, so it survives
//! model swaps. Architectures that don't emit `l_out` simply produce
//! empty keyframes — the visualization falls back gracefully.
//!
//! ## Threading and lifetime
//!
//! The callback may run on a ggml scheduler thread; all shared state is
//! atomics + mutexes. Arming happens strictly outside `llama_decode`
//! (which is synchronous), so the armed flag cannot change between the
//! `ask` and data phases of one node. The `user_data` pointer handed to
//! llama.cpp is a raw pointer to the tap's shared block; see
//! [`BrainTap::install`] for the lifetime contract.

use std::collections::VecDeque;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use llama_cpp_2::context::params::LlamaContextParams;

/// Bounded keyframe ring; oldest frames are dropped when the consumer
/// (overlay) lags. 256 frames ≈ over a minute of playback at the
/// highest keyframe rate the design calls for (~4/s).
pub const MAX_KEYFRAMES: usize = 256;

/// Per-tensor copy ceiling. `l_out` for a 4096-wide model is 16 KiB;
/// MoE router tensors are a few dozen bytes. Anything larger than this
/// is not a tensor we meant to tap (e.g. a batched prefill slipped
/// through) — skip it rather than pay for the copy.
const MAX_TAP_TENSOR_BYTES: usize = 64 * 1024;

/// ggml type ids we handle (from `ggml.h`; stable ABI values).
const GGML_TYPE_F32: u32 = 0;
const GGML_TYPE_I32: u32 = 26;

/// `llama_cpp_sys_2::ggml_type` is a bindgen type alias for the C enum's
/// underlying integer type, which is ABI-dependent: Itanium (Linux/macOS)
/// picks `unsigned int` for an all-non-negative enum, while the Microsoft
/// ABI (Windows/MSVC) always uses `int`. Comparing through `i64` sidesteps
/// the signedness split without caring which one the target picked.
fn ggml_type_is(t: llama_cpp_sys_2::ggml_type, expect: u32) -> bool {
    i64::from(t) == i64::from(expect)
}

/// Default sampling cadence: consider every 3rd generated token (the
/// governor widens this whenever the measured surcharge would break the
/// budget). Phase 2 replaces this with playback-paced demand.
pub const DEFAULT_BASE_INTERVAL: u32 = 3;

/// Hard decode-overhead budget for the tap (fraction; 0.01 = 1 %). See
/// the plan's verification criteria.
pub const OVERHEAD_BUDGET: f64 = 0.01;

/// Layer-observation stride: each keyframe captures `l_out` for only
/// every `LAYER_STRIDE`-th layer, with the phase rotating on every arm
/// so `LAYER_STRIDE` consecutive keyframes cover the full stack.
///
/// This exists because the surcharge of a sampled token is dominated
/// not by the tensor copies (a few KB) but by the graph-scheduler
/// splits each observed node forces — measured on gemma-4-e2b (35
/// layers, 8-core CPU): observing all layers cost ~30 % of a token,
/// observing a quarter of them cuts that near-proportionally. MoE
/// router tensors are *not* strided — expert activity is the headline
/// signal and those tensors are top-k-sized.
pub const LAYER_STRIDE: u32 = 4;

/// Process-wide capture latch, set from `[overlay].brain_capture` by the
/// daemon at startup and on config reload. The embedded-backend factories
/// read it when constructing a local LLM so the tap is compiled in but
/// dormant (callback never installed) unless the user opted in. A global
/// is used deliberately: the polish/assistant factories are invoked from
/// many call sites with narrow config slices (`&Polish` / `&AssistantCfg`)
/// that don't carry overlay settings, and threading one bool through every
/// signature would churn them all for a purely observational feature.
static CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable/disable brain-keyframe capture for local LLM backends built
/// *after* this call (existing backends keep the setting they were built
/// with). Called by the daemon from `[overlay].brain_capture`.
pub fn set_capture_enabled(enabled: bool) {
    CAPTURE_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Current value of the process-wide capture latch (read by the
/// embedded-backend factories at construction time).
#[must_use]
pub fn capture_enabled() -> bool {
    CAPTURE_ENABLED.load(Ordering::Relaxed)
}

/// One event on the brain-trace bus: the reply lifecycle plus every
/// captured keyframe, in decode order. Consumed by the Glass Cortex
/// replay engine (`fono-overlay`), which turns the generation-burst
/// trace into a playback-paced animation.
#[derive(Debug, Clone)]
pub enum BrainEvent {
    /// A local generation is starting (assistant reply or polish
    /// cleanup). `n_layer` is the loaded model's transformer depth.
    ReplyBegin { n_layer: u32 },
    /// One prompt-prefill batch finished decoding (`n_tokens` wide).
    /// A reply may prefill in several batches (cached prefix +
    /// suffix); the Glass Cortex fires one spine sweep per event —
    /// the prompt visibly flowing through the layers.
    Prefill { n_tokens: u32 },
    /// One sampled token's keyframe.
    Frame(BrainKeyframe),
    /// The generation finished. `total_tokens` is the number of
    /// decoded tokens (the keyframes' `token_index` domain), `gen_ms`
    /// the wall-clock decode duration; `ctx_used` / `ctx_capacity`
    /// describe KV-cache fill at the end of the reply.
    ReplyEnd { total_tokens: u64, gen_ms: u64, ctx_used: u32, ctx_capacity: u32 },
}

/// Consumer callback for [`BrainEvent`]s. Runs on the decode thread —
/// implementations MUST be cheap and non-blocking (the overlay sink
/// is an `mpsc` send + waker).
pub type BrainEventSink = Arc<dyn Fn(BrainEvent) + Send + Sync>;

/// Process-wide event sink, installed by the daemon when the overlay
/// exists. Same global-latch rationale as [`CAPTURE_ENABLED`]: the
/// publishers sit deep inside backend decode loops that have no
/// channel back to the orchestrator.
static EVENT_SINK: Mutex<Option<BrainEventSink>> = Mutex::new(None);

/// Install (or clear, with `None`) the process-wide brain-event sink.
pub fn set_event_sink(sink: Option<BrainEventSink>) {
    *EVENT_SINK.lock().expect("brain-tap sink mutex poisoned") = sink;
}

/// Snapshot the installed sink (cheap `Arc` clone; `None` when no
/// consumer is listening — the publish paths then skip all work).
#[must_use]
fn event_sink() -> Option<BrainEventSink> {
    EVENT_SINK.lock().expect("brain-tap sink mutex poisoned").clone()
}

/// Publish a reply-begin event for a generation observed by `tap`.
/// No-op when the tap is absent or no sink is installed.
pub fn publish_reply_begin(tap: Option<&BrainTap>) {
    if let (Some(tap), Some(sink)) = (tap, event_sink()) {
        sink(BrainEvent::ReplyBegin { n_layer: tap.n_layer() });
    }
}

/// Publish a prefill-batch event for a generation observed by `tap`
/// (call after the batch's `llama_decode` returns). No-op when the tap
/// is absent or no sink is installed.
pub fn publish_prefill(tap: Option<&BrainTap>, n_tokens: u32) {
    if tap.is_some() && n_tokens > 0 {
        if let Some(sink) = event_sink() {
            sink(BrainEvent::Prefill { n_tokens });
        }
    }
}

/// Publish a reply-end event for a generation observed by `tap`.
/// No-op when the tap is absent or no sink is installed.
pub fn publish_reply_end(
    tap: Option<&BrainTap>,
    total_tokens: u64,
    gen_ms: u64,
    ctx_used: u32,
    ctx_capacity: u32,
) {
    if tap.is_some() {
        if let Some(sink) = event_sink() {
            sink(BrainEvent::ReplyEnd { total_tokens, gen_ms, ctx_used, ctx_capacity });
        }
    }
}

/// Routed experts for one layer of one sampled token.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerExperts {
    /// Transformer layer index (0-based).
    pub layer: u32,
    /// Expert ids chosen by the router (top-k order).
    pub ids: Vec<i32>,
    /// Routing weights aligned with `ids`; may be empty if the weights
    /// tensor was not observed for this token.
    pub weights: Vec<f32>,
}

/// One sampled token's worth of forward-pass signals.
///
/// `layer_norms` has **partial coverage by design**: each keyframe
/// observes only every [`LAYER_STRIDE`]-th layer (rotating phase, see
/// [`BrainTap::arm`]), because every observed graph node forces a
/// scheduler split and the per-sample surcharge scales with node count.
/// Consecutive keyframes rotate through the phases so `LAYER_STRIDE`
/// frames assemble a full layer picture; consumers merge by treating
/// `0.0` as "not observed this frame".
#[derive(Debug, Clone, Default)]
pub struct BrainKeyframe {
    /// Index of the token within the current generation (0-based).
    pub token_index: u64,
    /// L2 norm of each layer's output hidden state, indexed by layer.
    /// `0.0` where the layer was not observed.
    pub layer_norms: Vec<f32>,
    /// MoE router choices per layer; empty for dense models.
    pub experts: Vec<LayerExperts>,
    /// Probability of the sampled token (filled by the decode loop from
    /// the sampler, not by the eval callback).
    pub token_prob: Option<f32>,
    /// Shannon entropy of the token distribution in bits (filled by the
    /// decode loop).
    pub entropy_bits: Option<f32>,
}

/// In-flight capture for the currently armed token.
#[derive(Default)]
struct Capture {
    layer_norms: Vec<f32>,
    experts: Vec<LayerExperts>,
    /// Scratch for tensor copies, reused across nodes to avoid
    /// per-node allocation once warm.
    scratch: Vec<u8>,
}

/// State shared between the owner ([`BrainTap`]) and the C callback.
struct TapShared {
    armed: AtomicBool,
    token_index: AtomicU64,
    /// Which residue class of layers (`layer % LAYER_STRIDE`) the
    /// current keyframe observes; rotated by [`BrainTap::arm`].
    layer_phase: AtomicU32,
    /// Monotonic arm counter driving the phase rotation.
    arm_seq: AtomicU64,
    n_layer: u32,
    capture: Mutex<Capture>,
    frames: Mutex<VecDeque<BrainKeyframe>>,
    governor: Mutex<SampleGovernor>,
}

/// Owner handle for the eval-callback tap. One per embedded backend
/// (assistant, polish); create with the model's layer count, install
/// into each context's params, arm per sampled token.
pub struct BrainTap {
    shared: Arc<TapShared>,
}

impl BrainTap {
    /// `n_layer` comes from the loaded model's GGUF metadata
    /// (`LlamaModel::n_layer()`); it sizes the per-keyframe norm vector.
    #[must_use]
    pub fn new(n_layer: u32) -> Self {
        let shared = Arc::new(TapShared {
            armed: AtomicBool::new(false),
            token_index: AtomicU64::new(0),
            layer_phase: AtomicU32::new(0),
            arm_seq: AtomicU64::new(0),
            n_layer,
            capture: Mutex::new(Capture::default()),
            frames: Mutex::new(VecDeque::with_capacity(MAX_KEYFRAMES)),
            governor: Mutex::new(SampleGovernor::new(DEFAULT_BASE_INTERVAL, OVERHEAD_BUDGET)),
        });
        Self { shared }
    }

    /// Whether the decode loop should arm the tap for the next token
    /// (delegates to the embedded [`SampleGovernor`]).
    #[must_use]
    pub fn should_sample(&self) -> bool {
        self.shared.governor.lock().expect("brain-tap governor mutex poisoned").should_sample()
    }

    /// Report a decoded token's cost to the governor (see
    /// [`SampleGovernor::on_token`]). `decode_time` must cover the whole
    /// tap surcharge for sampled tokens — arm, decode, collect, and any
    /// logits statistics — so the backoff sees the true cost.
    pub fn on_token(&self, sampled: bool, decode_time: Duration) {
        self.shared
            .governor
            .lock()
            .expect("brain-tap governor mutex poisoned")
            .on_token(sampled, decode_time);
    }

    /// Current effective sampling interval (telemetry/logging).
    #[must_use]
    pub fn interval(&self) -> u32 {
        self.shared.governor.lock().expect("brain-tap governor mutex poisoned").interval()
    }

    /// Within-run overhead estimate from the governor's EMAs — see
    /// [`SampleGovernor::overhead_estimate`].
    #[must_use]
    pub fn overhead_estimate(&self) -> Option<OverheadEstimate> {
        self.shared.governor.lock().expect("brain-tap governor mutex poisoned").overhead_estimate()
    }

    /// Install the tap into context params about to be passed to
    /// `LlamaModel::new_context`.
    ///
    /// # Safety
    ///
    /// The returned context holds a raw pointer to this tap's shared
    /// state: **`self` must outlive every context created from
    /// `params`**. In both embedded backends the tap lives on the
    /// backend struct and contexts are created and dropped inside its
    /// methods, which satisfies the contract by construction.
    pub unsafe fn install(&self, params: &mut LlamaContextParams) {
        // `LlamaContextParams` is a single-field newtype over the sys
        // struct (`pub(crate) context_params`); the wrapper offers no
        // accessor, so we go through a layout-asserted cast. The asserts
        // below turn any upstream layout change into a compile error.
        const _: () = assert!(
            std::mem::size_of::<LlamaContextParams>()
                == std::mem::size_of::<llama_cpp_sys_2::llama_context_params>()
        );
        const _: () = assert!(
            std::mem::align_of::<LlamaContextParams>()
                == std::mem::align_of::<llama_cpp_sys_2::llama_context_params>()
        );
        let raw: *mut llama_cpp_sys_2::llama_context_params = std::ptr::from_mut(params).cast();
        // SAFETY: layout equality asserted above; `raw` derives from a
        // valid `&mut` and is written before the borrow ends.
        unsafe {
            (*raw).cb_eval = Some(tap_eval_cb);
            (*raw).cb_eval_user_data = Arc::as_ptr(&self.shared).cast_mut().cast::<c_void>();
        }
    }

    /// Arm the tap for the next `llama_decode` call. Call immediately
    /// before decoding the single-token batch of a token the
    /// visualization wants a keyframe for. Each arm rotates the layer
    /// phase, so successive keyframes observe successive residue
    /// classes of layers (see [`LAYER_STRIDE`]).
    pub fn arm(&self, token_index: u64) {
        {
            let mut cap = self.shared.capture.lock().expect("brain-tap capture mutex poisoned");
            cap.layer_norms.clear();
            cap.layer_norms.resize(self.shared.n_layer as usize, 0.0);
            cap.experts.clear();
        }
        let seq = self.shared.arm_seq.fetch_add(1, Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)]
        self.shared.layer_phase.store((seq % u64::from(LAYER_STRIDE)) as u32, Ordering::Relaxed);
        self.shared.token_index.store(token_index, Ordering::Relaxed);
        self.shared.armed.store(true, Ordering::Release);
    }

    /// Disarm after the decode call and return the assembled keyframe
    /// (without sampler-side fields — the caller enriches `token_prob`
    /// / `entropy_bits` before pushing it via [`Self::push_frame`]).
    /// Returns `None` if nothing was captured (unknown architecture or
    /// the tap was never armed).
    pub fn disarm_and_collect(&self) -> Option<BrainKeyframe> {
        self.shared.armed.store(false, Ordering::Release);
        let mut cap = self.shared.capture.lock().expect("brain-tap capture mutex poisoned");
        let saw_norms = cap.layer_norms.iter().any(|&n| n != 0.0);
        let saw_experts = !cap.experts.is_empty();
        if !saw_norms && !saw_experts {
            return None;
        }
        let mut experts = std::mem::take(&mut cap.experts);
        experts.sort_by_key(|e| e.layer);
        Some(BrainKeyframe {
            token_index: self.shared.token_index.load(Ordering::Relaxed),
            layer_norms: std::mem::take(&mut cap.layer_norms),
            experts,
            token_prob: None,
            entropy_bits: None,
        })
    }

    /// Push a finished keyframe into the bounded ring (drop-oldest).
    pub fn push_frame(&self, frame: BrainKeyframe) {
        let mut frames = self.shared.frames.lock().expect("brain-tap frame mutex poisoned");
        if frames.len() == MAX_KEYFRAMES {
            frames.pop_front();
        }
        frames.push_back(frame);
    }

    /// Drain all pending keyframes (consumer side — the overlay feed).
    #[must_use]
    pub fn take_frames(&self) -> Vec<BrainKeyframe> {
        let mut frames = self.shared.frames.lock().expect("brain-tap frame mutex poisoned");
        frames.drain(..).collect()
    }

    /// Layer count the tap was created with.
    #[must_use]
    pub fn n_layer(&self) -> u32 {
        self.shared.n_layer
    }
}

/// Which tapped tensor a graph-node name denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapTensor {
    /// `l_out-<layer>` — layer output hidden state.
    LayerOut(u32),
    /// `ffn_moe_topk-<layer>` — routed expert ids (I32).
    MoeTopk(u32),
    /// `ffn_moe_weights-<layer>` — routing weights (F32).
    MoeWeights(u32),
}

/// Parse a graph callback name (`"<base>-<layer>"`) into a tapped
/// tensor. Exact base match only — `ffn_moe_weights_norm-3` and friends
/// must NOT match `ffn_moe_weights`.
fn parse_tap_name(name: &str) -> Option<TapTensor> {
    let (base, layer) = name.rsplit_once('-')?;
    if layer.is_empty() || !layer.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let layer: u32 = layer.parse().ok()?;
    match base {
        "l_out" => Some(TapTensor::LayerOut(layer)),
        "ffn_moe_topk" => Some(TapTensor::MoeTopk(layer)),
        "ffn_moe_weights" => Some(TapTensor::MoeWeights(layer)),
        _ => None,
    }
}

/// Copy a tensor's bytes to host memory. Uses the backend copy (which
/// handles device→host sync) when the tensor lives in a backend
/// buffer; falls back to a straight memcpy for host-resident tensors
/// (also the path unit tests exercise with fabricated tensors).
///
/// Returns `false` (leaving `out` empty) for tensors we refuse to
/// copy: oversized, no data, or not yet materialised.
unsafe fn copy_tensor_bytes(t: *const llama_cpp_sys_2::ggml_tensor, out: &mut Vec<u8>) -> bool {
    out.clear();
    // SAFETY: `t` is the live tensor llama.cpp just handed the callback.
    unsafe {
        let nbytes = llama_cpp_sys_2::ggml_nbytes(t);
        if nbytes == 0 || nbytes > MAX_TAP_TENSOR_BYTES {
            return false;
        }
        out.resize(nbytes, 0);
        if (*t).buffer.is_null() {
            if (*t).data.is_null() {
                out.clear();
                return false;
            }
            std::ptr::copy_nonoverlapping((*t).data.cast::<u8>(), out.as_mut_ptr(), nbytes);
        } else {
            llama_cpp_sys_2::ggml_backend_tensor_get(
                t.cast_mut(),
                out.as_mut_ptr().cast::<c_void>(),
                0,
                nbytes,
            );
        }
    }
    true
}

/// Number of "token columns" in a tapped tensor: the product of every
/// dimension past the first. Tapped decode tensors are `[width, 1]`;
/// anything else means a batched eval slipped through and we skip it
/// (the prefill sweep is driven from batch progress, not from the tap).
unsafe fn token_columns(t: *const llama_cpp_sys_2::ggml_tensor) -> i64 {
    // SAFETY: `t` is the live tensor llama.cpp just handed the callback.
    unsafe { (*t).ne[1] * (*t).ne[2] * (*t).ne[3] }
}

/// The `cb_eval` entry point handed to llama.cpp.
///
/// Contract (mirrors upstream `imatrix`): with `ask = true`, the return
/// value says whether we want this node's data; with `ask = false` the
/// data is available and the return value must be `true` to continue
/// scheduling.
#[allow(clippy::significant_drop_tightening)] // capture guard spans the whole data phase by design
unsafe extern "C" fn tap_eval_cb(
    t: *mut llama_cpp_sys_2::ggml_tensor,
    ask: bool,
    user_data: *mut c_void,
) -> bool {
    // SAFETY: `user_data` is the `TapShared` pointer installed by
    // `BrainTap::install`, whose contract keeps it alive for every
    // context using this callback; `t` is the live node tensor.
    let (shared, tensor_type, name) = unsafe {
        let shared = &*user_data.cast_const().cast::<TapShared>();
        // Disarmed steady state: one atomic load + branch per node.
        if !shared.armed.load(Ordering::Acquire) {
            return !ask;
        }
        let name_ptr: *const c_char = (*t).name.as_ptr();
        let Ok(name) = CStr::from_ptr(name_ptr).to_str() else {
            return !ask;
        };
        (shared, (*t).type_, name)
    };
    let Some(kind) = parse_tap_name(name) else {
        return !ask;
    };
    // Layer-strided `l_out` observation: skip layers outside this
    // keyframe's residue class *in the ask phase*, so the scheduler
    // never splits the graph for them (the whole point — see
    // [`LAYER_STRIDE`]).
    if let TapTensor::LayerOut(layer) = kind {
        if layer % LAYER_STRIDE != shared.layer_phase.load(Ordering::Relaxed) {
            return !ask;
        }
    }
    if ask {
        return true;
    }
    // SAFETY: `t` is live for the duration of the data phase.
    if unsafe { token_columns(t) } != 1 {
        return true;
    }
    let Ok(mut cap) = shared.capture.lock() else { return true };
    let cap = &mut *cap;
    match kind {
        TapTensor::LayerOut(layer) => {
            if !ggml_type_is(tensor_type, GGML_TYPE_F32) {
                return true;
            }
            let mut scratch = std::mem::take(&mut cap.scratch);
            // SAFETY: live tensor, data phase.
            if unsafe { copy_tensor_bytes(t, &mut scratch) } {
                let norm = scratch
                    .chunks_exact(4)
                    .map(|c| f64::from(f32::from_ne_bytes([c[0], c[1], c[2], c[3]])))
                    .map(|x| x * x)
                    .sum::<f64>()
                    .sqrt();
                if let Some(slot) = cap.layer_norms.get_mut(layer as usize) {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        *slot = norm as f32;
                    }
                }
            }
            cap.scratch = scratch;
        }
        TapTensor::MoeTopk(layer) => {
            if !ggml_type_is(tensor_type, GGML_TYPE_I32) {
                return true;
            }
            let mut scratch = std::mem::take(&mut cap.scratch);
            // SAFETY: live tensor, data phase.
            if unsafe { copy_tensor_bytes(t, &mut scratch) } {
                let ids: Vec<i32> = scratch
                    .chunks_exact(4)
                    .map(|c| i32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                entry_for_layer(&mut cap.experts, layer).ids = ids;
            }
            cap.scratch = scratch;
        }
        TapTensor::MoeWeights(layer) => {
            if !ggml_type_is(tensor_type, GGML_TYPE_F32) {
                return true;
            }
            let mut scratch = std::mem::take(&mut cap.scratch);
            // SAFETY: live tensor, data phase.
            if unsafe { copy_tensor_bytes(t, &mut scratch) } {
                entry_for_layer(&mut cap.experts, layer).weights = scratch
                    .chunks_exact(4)
                    .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
            }
            cap.scratch = scratch;
        }
    }
    true
}

fn entry_for_layer(experts: &mut Vec<LayerExperts>, layer: u32) -> &mut LayerExperts {
    if let Some(pos) = experts.iter().position(|e| e.layer == layer) {
        return &mut experts[pos];
    }
    experts.push(LayerExperts { layer, ids: Vec::new(), weights: Vec::new() });
    experts.last_mut().expect("just pushed")
}

/// Decode one single-token batch, capturing a Glass Cortex keyframe when
/// the tap's governor elects this token. The timed window covers arm +
/// decode + collect + logits statistics so the governor's < 1 % backoff
/// sees the true per-sample surcharge. With `tap = None` this is exactly
/// `ctx.decode(batch)` — the shared decode-loop shape for both embedded
/// backends (assistant and polish).
///
/// # Errors
///
/// Propagates the underlying `llama_decode` failure untouched.
pub fn decode_token_with_tap(
    ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
    batch: &mut llama_cpp_2::llama_batch::LlamaBatch,
    tap: Option<&BrainTap>,
    token_index: u64,
) -> std::result::Result<(), llama_cpp_2::DecodeError> {
    let Some(tap) = tap else {
        return ctx.decode(batch);
    };
    let sample_this = tap.should_sample();
    let started = std::time::Instant::now();
    if sample_this {
        tap.arm(token_index);
    }
    ctx.decode(batch)?;
    if sample_this {
        if let Some(mut frame) = tap.disarm_and_collect() {
            let (prob, entropy) = logits_stats(ctx.get_logits_ith(0));
            frame.token_prob = Some(prob);
            frame.entropy_bits = Some(entropy);
            // Forward to the live event bus (the overlay replay
            // engine) when a consumer is installed, and always keep
            // the frame in the tap's own ring (bench + future
            // "replay this answer" trace persistence).
            if let Some(sink) = event_sink() {
                sink(BrainEvent::Frame(frame.clone()));
            }
            tap.push_frame(frame);
        }
    }
    tap.on_token(sample_this, started.elapsed());
    Ok(())
}

/// Confidence statistics over a raw logits slice: `(max_prob,
/// entropy_bits)` of the softmax distribution — the model's confidence
/// in its most likely next token and the overall uncertainty of the
/// distribution, in bits. Numerically stable (max-subtracted softmax);
/// runs on the decode thread only for sampled keyframe tokens, and its
/// cost is part of the timed surcharge the governor amortises.
#[must_use]
pub fn logits_stats(logits: &[f32]) -> (f32, f32) {
    if logits.is_empty() {
        return (0.0, 0.0);
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        return (0.0, 0.0);
    }
    let mut sum = 0.0_f64;
    for &l in logits {
        sum += f64::from(l - max).exp();
    }
    if sum <= 0.0 {
        return (0.0, 0.0);
    }
    let ln_sum = sum.ln();
    // H = -Σ p ln p  with  p = exp(l - max) / sum
    //   = ln_sum - Σ (l - max) p   (in nats)
    let mut weighted = 0.0_f64;
    for &l in logits {
        let d = f64::from(l - max);
        weighted = d.mul_add(d.exp() / sum, weighted);
    }
    let entropy_nats = ln_sum - weighted;
    let max_prob = 1.0 / sum; // exp(0) / sum for the max logit
    #[allow(clippy::cast_possible_truncation)]
    {
        (max_prob as f32, (entropy_nats / std::f64::consts::LN_2).max(0.0) as f32)
    }
}

/// Adaptive sampling governor enforcing the < 1 % decode-overhead
/// budget.
///
/// The decode loop reports every token's decode duration and whether
/// the tap was armed for it. The governor keeps exponential moving
/// averages of both populations and widens the sampling interval when
/// the measured per-sample surcharge, amortised over the interval,
/// would exceed the budget. It never shrinks below the caller's base
/// interval (the keyframe rate the visualization actually needs).
pub struct SampleGovernor {
    /// Overhead budget as a fraction (0.01 = 1 %).
    budget: f64,
    /// Interval requested by the consumer (sample every Nth token).
    base_interval: u32,
    /// Current effective interval (≥ `base_interval`).
    interval: u32,
    /// EMA of unsampled token decode seconds.
    plain_ema: Option<f64>,
    /// EMA of sampled token decode seconds.
    sampled_ema: Option<f64>,
    /// Tokens since the last sample.
    since_sample: u32,
}

/// EMA smoothing factor — light smoothing, reacts within a few tokens.
const EMA_ALPHA: f64 = 0.2;

/// Snapshot of the governor's cost model — see
/// [`SampleGovernor::overhead_estimate`].
#[derive(Debug, Clone, Copy)]
pub struct OverheadEstimate {
    /// EMA of an unsampled token's decode seconds.
    pub plain_s: f64,
    /// EMA of a sampled token's decode seconds (incl. the whole tap
    /// surcharge window).
    pub sampled_s: f64,
    /// Steady-state fractional slowdown the current interval implies:
    /// `max(sampled − plain, 0) / (interval × plain)`.
    pub amortized: f64,
}

impl SampleGovernor {
    /// `base_interval` = sample every Nth token when the budget allows
    /// (≥ 1); `budget` = allowed fractional slowdown (e.g. `0.01`).
    #[must_use]
    pub fn new(base_interval: u32, budget: f64) -> Self {
        Self {
            budget,
            base_interval: base_interval.max(1),
            interval: base_interval.max(1),
            plain_ema: None,
            sampled_ema: None,
            since_sample: 0,
        }
    }

    /// Should the tap be armed for the *next* token?
    #[must_use]
    pub fn should_sample(&self) -> bool {
        self.since_sample + 1 >= self.interval
    }

    /// Report a decoded token: whether it was sampled and how long the
    /// decode took. Updates the EMAs and re-derives the interval.
    pub fn on_token(&mut self, sampled: bool, decode_time: Duration) {
        let secs = decode_time.as_secs_f64();
        let ema = if sampled { &mut self.sampled_ema } else { &mut self.plain_ema };
        *ema = Some(ema.map_or(secs, |prev| EMA_ALPHA.mul_add(secs - prev, prev)));
        if sampled {
            self.since_sample = 0;
        } else {
            self.since_sample = self.since_sample.saturating_add(1);
        }
        self.rederive_interval();
    }

    /// Current effective interval (for logging/telemetry).
    #[must_use]
    pub fn interval(&self) -> u32 {
        self.interval
    }

    /// Within-run overhead estimate: both EMAs come from the *same*
    /// decode run, so the derived amortized slowdown is immune to the
    /// run-to-run thermal drift that makes A/B wall-clock comparisons
    /// on a throttling laptop unreliable. `None` until at least one
    /// sampled and one unsampled token have been reported.
    #[must_use]
    pub fn overhead_estimate(&self) -> Option<OverheadEstimate> {
        let (plain, sampled) = (self.plain_ema?, self.sampled_ema?);
        if plain <= 0.0 {
            return None;
        }
        let surcharge = (sampled - plain).max(0.0);
        Some(OverheadEstimate {
            plain_s: plain,
            sampled_s: sampled,
            amortized: surcharge / (f64::from(self.interval) * plain),
        })
    }

    fn rederive_interval(&mut self) {
        let (Some(plain), Some(sampled)) = (self.plain_ema, self.sampled_ema) else {
            self.interval = self.base_interval;
            return;
        };
        let surcharge = (sampled - plain).max(0.0);
        if surcharge == 0.0 || plain <= 0.0 {
            self.interval = self.base_interval;
            return;
        }
        // Amortised overhead of sampling every Nth token is
        // surcharge / (N * plain); solve for the smallest N within
        // budget.
        let min_interval = (surcharge / (self.budget * plain)).ceil();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_interval = if min_interval.is_finite() { min_interval as u32 } else { u32::MAX };
        self.interval = min_interval.max(self.base_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_name_parsing() {
        assert_eq!(parse_tap_name("l_out-0"), Some(TapTensor::LayerOut(0)));
        assert_eq!(parse_tap_name("l_out-47"), Some(TapTensor::LayerOut(47)));
        assert_eq!(parse_tap_name("ffn_moe_topk-3"), Some(TapTensor::MoeTopk(3)));
        assert_eq!(parse_tap_name("ffn_moe_weights-12"), Some(TapTensor::MoeWeights(12)));
        // Exact-base matches only.
        assert_eq!(parse_tap_name("ffn_moe_weights_norm-12"), None);
        assert_eq!(parse_tap_name("ffn_moe_probs-3"), None);
        assert_eq!(parse_tap_name("ffn_moe_topk"), None);
        assert_eq!(parse_tap_name("l_out-"), None);
        assert_eq!(parse_tap_name("l_out-x"), None);
        assert_eq!(parse_tap_name("attn_norm-4"), None);
        assert_eq!(parse_tap_name("result_output"), None);
    }

    /// Build a host-resident fabricated tensor the callback can read
    /// through its memcpy path (null backend buffer).
    fn fake_tensor(
        name: &str,
        type_: u32,
        ne0: i64,
        data: *mut c_void,
    ) -> llama_cpp_sys_2::ggml_tensor {
        // SAFETY: zeroed is a valid bit pattern for this C struct.
        let mut t: llama_cpp_sys_2::ggml_tensor = unsafe { std::mem::zeroed() };
        // `ggml_type` is `c_uint` or `c_int` depending on target ABI (see
        // `ggml_type_is` above); this bit-preserving cast is exact for the
        // small non-negative ids this test helper is ever called with.
        #[allow(clippy::cast_possible_wrap)]
        {
            t.type_ = type_ as llama_cpp_sys_2::ggml_type;
        }
        t.ne = [ne0, 1, 1, 1];
        let el = if type_ == GGML_TYPE_I32 || type_ == GGML_TYPE_F32 { 4 } else { 0 };
        t.nb = [el, el * ne0 as usize, el * ne0 as usize, el * ne0 as usize];
        t.data = data;
        for (dst, src) in t.name.iter_mut().zip(name.as_bytes()) {
            #[allow(clippy::cast_possible_wrap)]
            {
                *dst = *src as c_char;
            }
        }
        t
    }

    /// Drive the raw callback exactly the way llama.cpp does (ask
    /// phase, then data phase) and check the assembled keyframe.
    #[test]
    #[allow(clippy::float_cmp)] // exact 0.0 marks "layer not observed this frame"
    #[allow(clippy::cognitive_complexity)] // linear test: arrange phases, assert keyframe
    fn callback_captures_norms_and_experts_when_armed() {
        let tap = BrainTap::new(2);
        let user_data = Arc::as_ptr(&tap.shared).cast_mut().cast::<c_void>();

        // Disarmed: not interested, and eval continues.
        let mut l0 = [3.0f32, 4.0f32];
        let mut t = fake_tensor("l_out-0", GGML_TYPE_F32, 2, l0.as_mut_ptr().cast());
        unsafe {
            assert!(!tap_eval_cb(&raw mut t, true, user_data));
            assert!(tap_eval_cb(&raw mut t, false, user_data));
        }
        assert!(tap.disarm_and_collect().is_none());

        // Armed: first arm observes phase 0 (layers ≡ 0 mod
        // LAYER_STRIDE), so `l_out-0` is captured and `l_out-1` is
        // declined in the ask phase; MoE tensors are never strided.
        tap.arm(7);
        unsafe {
            assert!(tap_eval_cb(&raw mut t, true, user_data));
            assert!(tap_eval_cb(&raw mut t, false, user_data));

            let mut l1 = [6.0f32, 8.0f32];
            let mut t1 = fake_tensor("l_out-1", GGML_TYPE_F32, 2, l1.as_mut_ptr().cast());
            assert!(!tap_eval_cb(&raw mut t1, true, user_data), "off-phase layer declined");

            let mut ids = [5i32, 2i32];
            let mut tk = fake_tensor("ffn_moe_topk-1", GGML_TYPE_I32, 2, ids.as_mut_ptr().cast());
            assert!(tap_eval_cb(&raw mut tk, true, user_data));
            assert!(tap_eval_cb(&raw mut tk, false, user_data));

            let mut w = [0.7f32, 0.3f32];
            let mut tw = fake_tensor("ffn_moe_weights-1", GGML_TYPE_F32, 2, w.as_mut_ptr().cast());
            assert!(tap_eval_cb(&raw mut tw, true, user_data));
            assert!(tap_eval_cb(&raw mut tw, false, user_data));

            // A tensor we never tap stays uninteresting even armed.
            let mut other = fake_tensor("attn_norm-0", GGML_TYPE_F32, 2, l0.as_mut_ptr().cast());
            assert!(!tap_eval_cb(&raw mut other, true, user_data));
        }
        let frame = tap.disarm_and_collect().expect("keyframe captured");
        assert_eq!(frame.token_index, 7);
        assert_eq!(frame.layer_norms.len(), 2);
        assert!((frame.layer_norms[0] - 5.0).abs() < 1e-5); // |(3,4)| = 5
        assert_eq!(frame.layer_norms[1], 0.0, "off-phase layer not observed");
        assert_eq!(frame.experts.len(), 1);
        assert_eq!(frame.experts[0].layer, 1);
        assert_eq!(frame.experts[0].ids, vec![5, 2]);
        assert_eq!(frame.experts[0].weights, vec![0.7, 0.3]);

        // Second collect without re-arming yields nothing.
        assert!(tap.disarm_and_collect().is_none());

        // Second arm rotates to phase 1: now `l_out-1` is observed and
        // `l_out-0` is declined.
        tap.arm(8);
        unsafe {
            assert!(!tap_eval_cb(&raw mut t, true, user_data), "phase rotated off layer 0");
            let mut l1 = [6.0f32, 8.0f32];
            let mut t1 = fake_tensor("l_out-1", GGML_TYPE_F32, 2, l1.as_mut_ptr().cast());
            assert!(tap_eval_cb(&raw mut t1, true, user_data));
            assert!(tap_eval_cb(&raw mut t1, false, user_data));
        }
        let frame = tap.disarm_and_collect().expect("second keyframe");
        assert_eq!(frame.layer_norms[0], 0.0);
        assert!((frame.layer_norms[1] - 10.0).abs() < 1e-5); // |(6,8)| = 10
    }

    #[test]
    fn callback_skips_batched_and_wrong_type_tensors() {
        let tap = BrainTap::new(1);
        let user_data = Arc::as_ptr(&tap.shared).cast_mut().cast::<c_void>();
        tap.arm(0);
        unsafe {
            // Batched (n_tokens = 2): data phase skips.
            let mut vals = [1.0f32, 2.0, 3.0, 4.0];
            let mut t = fake_tensor("l_out-0", GGML_TYPE_F32, 2, vals.as_mut_ptr().cast());
            t.ne[1] = 2;
            assert!(tap_eval_cb(&raw mut t, true, user_data));
            assert!(tap_eval_cb(&raw mut t, false, user_data));

            // Wrong dtype for topk.
            let mut w = [0.5f32];
            let mut tw = fake_tensor("ffn_moe_topk-0", GGML_TYPE_F32, 1, w.as_mut_ptr().cast());
            assert!(tap_eval_cb(&raw mut tw, true, user_data));
            assert!(tap_eval_cb(&raw mut tw, false, user_data));
        }
        assert!(tap.disarm_and_collect().is_none());
    }

    #[test]
    fn frame_ring_drops_oldest() {
        let tap = BrainTap::new(1);
        for i in 0..(MAX_KEYFRAMES + 3) {
            tap.push_frame(BrainKeyframe { token_index: i as u64, ..Default::default() });
        }
        let frames = tap.take_frames();
        assert_eq!(frames.len(), MAX_KEYFRAMES);
        assert_eq!(frames[0].token_index, 3);
        assert!(tap.take_frames().is_empty());
    }

    #[test]
    fn governor_widens_interval_to_hold_budget() {
        // Plain token: 50 ms. Sampled token: 60 ms (20 % surcharge).
        // Budget 1 % ⇒ need interval ≥ 0.010 / (0.01 * 0.050) = 20.
        let mut g = SampleGovernor::new(5, 0.01);
        for _ in 0..50 {
            g.on_token(false, Duration::from_millis(50));
        }
        assert!(g.should_sample(), "base interval elapsed");
        g.on_token(true, Duration::from_millis(60));
        for _ in 0..30 {
            g.on_token(false, Duration::from_millis(50));
        }
        assert!(g.interval() >= 20, "interval {} should be >= 20", g.interval());

        // Cheap sampling relaxes back to the base interval.
        let mut g = SampleGovernor::new(5, 0.01);
        g.on_token(false, Duration::from_millis(50));
        g.on_token(true, Duration::from_millis(50));
        assert_eq!(g.interval(), 5);
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact zeros are the documented degenerate returns
    fn logits_stats_uniform_and_peaked() {
        // Uniform over 4 tokens: p_max = 0.25, H = 2 bits.
        let (p, h) = logits_stats(&[0.0, 0.0, 0.0, 0.0]);
        assert!((p - 0.25).abs() < 1e-6);
        assert!((h - 2.0).abs() < 1e-5);

        // Strongly peaked: near-certain, near-zero entropy.
        let (p, h) = logits_stats(&[20.0, 0.0, 0.0, 0.0]);
        assert!(p > 0.999);
        assert!(h < 0.01);

        // Degenerate inputs don't panic.
        assert_eq!(logits_stats(&[]), (0.0, 0.0));
        let (p, _) = logits_stats(&[f32::NEG_INFINITY, f32::NEG_INFINITY]);
        assert_eq!(p, 0.0);
    }

    #[test]
    fn governor_samples_on_schedule() {
        let mut g = SampleGovernor::new(3, 0.01);
        let mut pattern = Vec::new();
        for _ in 0..9 {
            let s = g.should_sample();
            pattern.push(s);
            g.on_token(s, Duration::from_millis(10));
        }
        assert_eq!(pattern.iter().filter(|&&s| s).count(), 3);
    }
}
