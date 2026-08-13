// SPDX-License-Identifier: GPL-3.0-only
//! Decides whether a model runs on an accelerator or on the host.
//!
//! The answer is all-or-nothing, and it is recomputed at every load.
//!
//! All-or-nothing because a partial offload is not a partial win. Reading a
//! prompt does scale with the fraction of the model moved onto the device,
//! since it submits hundreds of tokens per call and the hop between devices is
//! amortised across all of them. Generating submits one token per call and pays
//! that hop on every token, so on an Arc 140V half the layers generated
//! *slower* than none at all (6.07 against 8.53 tokens a second, measured
//! 2026-08-11). Generation is what a dictation reply is made of, so there is no
//! polite middle setting to pick: either the whole model goes on the device or
//! none of it does, and the memory budget decides which.
//!
//! Recomputed every time because the answer is only true for the moment it was
//! measured. It fit when twenty gigabytes were free; it does not fit once a
//! browser has taken half the machine. Reading the driver's figure again costs
//! one call, which is cheaper than being wrong in the direction that pins
//! memory the desktop needs.
//!
//! The estimate is deliberately coarse — weights from the files on disk, the KV
//! cache from the model's own hyperparameters, and a flat allowance for the
//! compute buffers. It only has to answer "does the whole thing fit", and a
//! load that fails anyway falls back to the host (see
//! [`crate::llama_backend::shared_model_sized`]).

use std::path::Path;

use llama_cpp_2::context::params::KvCacheType;
use tracing::debug;

use crate::ggml_devices::{devices, GgmlDevice, GgmlDeviceKind};

/// Allowance for everything resident on the device that is neither weights nor
/// KV cache: the graph compute buffers, and slack for an estimate made from
/// file sizes. A full offload of a 26B model measured 523 MiB of compute
/// buffers against 9,141 MiB of weights (2026-08-11), so a round gigabyte
/// covers that with room for a bigger batch.
const COMPUTE_HEADROOM_BYTES: u64 = 1024 * 1024 * 1024;

/// Memory left to the rest of the machine when the device draws on system RAM.
///
/// On such a device the "free memory" figure *is* system memory — this laptop's
/// iGPU offers 23 GB on a 30 GB host — and every offloaded byte turns evictable
/// page cache into memory pinned until the model unloads. Nothing errors when
/// that goes wrong; the desktop simply gets slower, which is why the reserve
/// exists rather than relying on a failed allocation to tell us.
const DESKTOP_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// What a model needs and where it is going to run.
#[derive(Debug, Clone)]
pub struct OffloadDecision {
    /// Layers to hand llama.cpp. `0` keeps the model on the host.
    pub n_gpu_layers: u32,
    /// One line naming the device and the numbers behind the answer, for logs
    /// and diagnostics.
    pub explanation: String,
}

impl OffloadDecision {
    fn host_only(explanation: String) -> Self {
        Self { n_gpu_layers: 0, explanation }
    }
}

/// Decide where the model at `model_path` should run, for a context of `n_ctx`
/// tokens with the given KV cache types.
///
/// Never fails: anything unmeasurable — no accelerator, unreadable model files,
/// hyperparameters that will not load — keeps the model on the host, which is
/// always a correct answer and only ever costs speed.
#[must_use]
pub fn decide(
    model_path: &Path,
    n_ctx: u32,
    type_k: KvCacheType,
    type_v: KvCacheType,
) -> OffloadDecision {
    let Some(device) = target_device() else {
        return OffloadDecision::host_only("no accelerator registered".to_string());
    };
    let Some(shape) = ModelShape::probe(model_path) else {
        return OffloadDecision::host_only(format!(
            "{}: could not read the model's size, staying on the CPU",
            device.description
        ));
    };
    let kv = shape.kv_bytes(n_ctx.max(1), type_k, type_v);
    let needed = shape.weights_bytes.saturating_add(kv).saturating_add(COMPUTE_HEADROOM_BYTES);
    let budget = budget_bytes(&device);
    let fits = needed <= budget;
    let explanation = format!(
        "{}: model needs {} ({} weights + {} cache + {} working), {} available — {}",
        device.description,
        gb(needed),
        gb(shape.weights_bytes),
        gb(kv),
        gb(COMPUTE_HEADROOM_BYTES),
        gb(budget),
        if fits { "running on the device" } else { "running on the CPU" }
    );
    // `n_layer + 1` is the output layer on top of the repeating blocks, which
    // is how llama.cpp counts a full offload.
    OffloadDecision { n_gpu_layers: if fits { shape.n_layer() + 1 } else { 0 }, explanation }
}

/// The device llama.cpp would load onto: a dedicated card if there is one,
/// otherwise an integrated GPU. Mirrors llama.cpp's own preference, and skips
/// the CPU and helper devices, which have nothing to offload to.
fn target_device() -> Option<GgmlDevice> {
    let all = devices();
    let pick = |kind: GgmlDeviceKind| all.iter().find(|d| d.kind == kind).cloned();
    pick(GgmlDeviceKind::Gpu).or_else(|| pick(GgmlDeviceKind::IGpu))
}

/// How much of the device we are willing to fill.
///
/// A dedicated card reporting a real budget is believed as-is: the memory is a
/// separate pool, so taking it costs the desktop nothing. Otherwise — an
/// integrated GPU, or a driver that answered with its whole heap because it
/// does not implement the budget query — the figure is additionally bound by
/// free system RAM less the desktop's reserve. A platform that cannot report
/// its free memory therefore offers no budget at all and the model stays on the
/// host: the alternative is pinning memory we never established was there.
fn budget_bytes(device: &GgmlDevice) -> u64 {
    if device.free_is_trustworthy() {
        return device.free_bytes;
    }
    let host = crate::hwcheck::available_ram_bytes().saturating_sub(DESKTOP_RESERVE_BYTES);
    device.free_bytes.min(host)
}

/// The parts of a model that decide whether it fits.
struct ModelShape {
    /// One entry per repeating block. Blocks are not interchangeable: a model
    /// can vary its key/value head count layer by layer, and give its
    /// sliding-window layers narrower heads than its full-attention ones.
    blocks: Vec<KvBlock>,
    /// Every shard on disk, which is what the weights occupy once resident.
    weights_bytes: u64,
}

/// What one block costs to remember: how many key/value heads it keeps and how
/// wide a key and a value row are. A recurrent block keeps none of that — it
/// carries a fixed-size state instead, so it costs nothing per token.
#[derive(Clone)]
struct KvBlock {
    n_head_kv: u64,
    k_dim: i64,
    v_dim: i64,
    recurrent: bool,
}

impl ModelShape {
    /// Read the shape from the GGUF's own metadata table, without loading the
    /// model.
    ///
    /// A `vocab_only` model load looks like the obvious way to ask these
    /// questions and is a trap: llama.cpp returns from its hyperparameter reader
    /// *before* the attention keys when `vocab_only` is set, leaving the layer
    /// count at zero, and the accessor for the head count then calls
    /// `GGML_ABORT` rather than returning an error — the process dies during a
    /// sizing decision. Reading the key-value table directly is both cheaper and
    /// the only version that cannot abort.
    fn probe(model_path: &Path) -> Option<Self> {
        let weights_bytes = crate::prompt_cache::model_shard_paths(model_path)
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .sum();
        if weights_bytes == 0 {
            return None;
        }
        let meta = GgufMeta::open(model_path)?;
        // Keys are namespaced by architecture, so the architecture comes first.
        let arch = meta.string("general.architecture")?;
        let key = |suffix: &str| format!("{arch}.{suffix}");
        let n_layer = usize::try_from(meta.integer(&key("block_count"))?).ok()?;
        // Head size is its own key because it is not always `n_embd / n_head` —
        // this Gemma spreads 2,816 across 16 heads of 512, not 176. Fall back to
        // the division only when the key is absent.
        let n_head = meta.integer(&key("attention.head_count")).unwrap_or(1).max(1);
        let even_split = meta.integer(&key("embedding_length")).unwrap_or(0) / n_head;
        let dim = |suffix: &str, fallback: i64| {
            meta.integer(&key(suffix))
                .and_then(|d| i64::try_from(d).ok())
                .filter(|d| *d > 0)
                .unwrap_or(fallback)
        };
        let even_split = i64::try_from(even_split).unwrap_or(1).max(1);
        let k_dim = dim("attention.key_length", even_split);
        let v_dim = dim("attention.value_length", even_split);
        // Sliding-window layers can carry narrower heads than full-attention
        // ones. Absent keys mean "same as the full-attention layers".
        let k_swa = dim("attention.key_length_swa", k_dim);
        let v_swa = dim("attention.value_length_swa", v_dim);
        let heads = meta.integers(&key("attention.head_count_kv")).unwrap_or_default();
        let swa = meta.bools(&key("attention.sliding_window_pattern")).unwrap_or_default();
        // A hybrid model replaces most of its attention blocks with recurrent
        // ones. Either the file lists them, or it gives the interval at which
        // attention blocks recur — every block but the last of each group is
        // recurrent. The interval key is only honoured when present, since a
        // model without it has no recurrent blocks at all.
        let recurrent = meta.bools(&key("attention.recurrent_layers")).unwrap_or_default();
        let attn_every = meta.integer(&key("full_attention_interval")).filter(|i| *i > 0);
        let blocks = (0..n_layer)
            .map(|il| {
                // A scalar key broadcasts to every block; an array is read per
                // block. Missing entirely means one head, which under-counts,
                // but a model that does not say how many heads it keeps also
                // gives us nothing better to guess with.
                let n_head_kv = heads.get(il).or_else(|| heads.first()).copied().unwrap_or(1);
                let windowed = swa.get(il).copied().unwrap_or(false);
                let recurrent = recurrent.get(il).copied().unwrap_or_else(|| {
                    attn_every.is_some_and(|every| (il as u64 + 1) % every != 0)
                });
                KvBlock {
                    n_head_kv,
                    k_dim: if windowed { k_swa } else { k_dim },
                    v_dim: if windowed { v_swa } else { v_dim },
                    recurrent,
                }
            })
            .collect();
        Some(Self { blocks, weights_bytes })
    }

    /// Repeating blocks, excluding the output layer.
    fn n_layer(&self) -> u32 {
        u32::try_from(self.blocks.len()).unwrap_or(0)
    }

    /// Bytes the KV cache occupies at full context.
    ///
    /// Every block is costed separately because their costs differ, and the
    /// difference is not small: on this Gemma the sliding-window blocks keep
    /// eight 256-wide heads while the full-attention ones keep two 512-wide, so
    /// a per-layer average would be wrong by a factor of two.
    ///
    /// Sliding-window blocks are costed at full context because Fono allocates
    /// them that way — a windowed cache cannot serve the saved-state cache.
    /// Recurrent blocks are costed at nothing: their state is a fixed size,
    /// which the working-memory allowance covers — on Qwen3.6-35B, where three
    /// blocks in four are recurrent, that state measured about 64 MiB against
    /// the round gigabyte allowed (2026-08-11). Counting them as attention
    /// blocks instead would have overstated that model's cache four times over.
    ///
    /// `ggml_row_size` is asked rather than assumed so a quantized cache is
    /// costed with its block overhead included: `q8_0` is a byte per element
    /// plus a scale every 32, not a flat byte.
    fn kv_bytes(&self, n_ctx: u32, type_k: KvCacheType, type_v: KvCacheType) -> u64 {
        let row = |ty: KvCacheType, dim: i64| {
            // SAFETY: a pure size calculation over ggml's static type table;
            // it touches no state and takes no pointers.
            let bytes = unsafe { llama_cpp_sys_2::ggml_row_size(ty.into(), dim) };
            bytes as u64
        };
        let per_token: u64 = self
            .blocks
            .iter()
            .filter(|b| !b.recurrent)
            .map(|b| b.n_head_kv * (row(type_k, b.k_dim).saturating_add(row(type_v, b.v_dim))))
            .sum();
        u64::from(n_ctx) * per_token
    }
}

/// Read-only view of a GGUF file's key-value table.
///
/// Opened with `no_alloc`, so the tensor data is never read — only the header
/// and metadata at the front of the file, which is what makes this affordable on
/// a model of a hundred gigabytes.
struct GgufMeta(*mut llama_cpp_sys_2::gguf_context);

impl GgufMeta {
    fn open(path: &Path) -> Option<Self> {
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        let params =
            llama_cpp_sys_2::gguf_init_params { no_alloc: true, ctx: std::ptr::null_mut() };
        // SAFETY: a valid null-terminated path, and `no_alloc` with a null
        // context out-pointer, which is the documented way to ask for metadata
        // only. Returns null on anything it cannot parse.
        let ctx = unsafe { llama_cpp_sys_2::gguf_init_from_file(c_path.as_ptr(), params) };
        if ctx.is_null() {
            debug!("offload sizing: {} is not readable as a GGUF", path.display());
            return None;
        }
        Some(Self(ctx))
    }

    fn key_id(&self, key: &str) -> Option<i64> {
        let c_key = std::ffi::CString::new(key).ok()?;
        // SAFETY: live context from `open`, valid null-terminated key.
        let id = unsafe { llama_cpp_sys_2::gguf_find_key(self.0, c_key.as_ptr()) };
        (id >= 0).then_some(id)
    }

    fn string(&self, key: &str) -> Option<String> {
        let id = self.key_id(key)?;
        // SAFETY: live context and an id `gguf_find_key` returned.
        let ty = unsafe { llama_cpp_sys_2::gguf_get_kv_type(self.0, id) };
        if ty != llama_cpp_sys_2::GGUF_TYPE_STRING {
            return None;
        }
        // SAFETY: the type check above is what the accessor asserts on; the
        // returned string is owned by the context and copied out immediately.
        unsafe {
            let ptr = llama_cpp_sys_2::gguf_get_val_str(self.0, id);
            (!ptr.is_null()).then(|| std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned())
        }
    }

    /// An integer-valued key, whatever width the file wrote it as.
    ///
    /// The accessors abort on a type mismatch rather than returning, so the type
    /// is checked first and an unexpected one reads as absent.
    fn integer(&self, key: &str) -> Option<u64> {
        let id = self.key_id(key)?;
        // SAFETY: live context and an id `gguf_find_key` returned. Every
        // accessor below is guarded by the type it asserts on.
        unsafe {
            match llama_cpp_sys_2::gguf_get_kv_type(self.0, id) {
                llama_cpp_sys_2::GGUF_TYPE_UINT32 => {
                    Some(u64::from(llama_cpp_sys_2::gguf_get_val_u32(self.0, id)))
                }
                llama_cpp_sys_2::GGUF_TYPE_INT32 => {
                    u64::try_from(llama_cpp_sys_2::gguf_get_val_i32(self.0, id)).ok()
                }
                llama_cpp_sys_2::GGUF_TYPE_UINT64 => {
                    Some(llama_cpp_sys_2::gguf_get_val_u64(self.0, id))
                }
                _ => None,
            }
        }
    }

    /// A key that holds one integer per layer, or one integer for every layer.
    ///
    /// Per-layer arrays are how a model says its blocks differ — this Gemma
    /// writes `[8, 8, 8, 8, 8, 2, …]` for its key/value head counts — so
    /// collapsing them to a single number is exactly the error worth avoiding.
    /// A scalar comes back as a one-element list, which the caller broadcasts.
    fn integers(&self, key: &str) -> Option<Vec<u64>> {
        if let Some(one) = self.integer(key) {
            return Some(vec![one]);
        }
        let id = self.key_id(key)?;
        // SAFETY: live context and an id `gguf_find_key` returned; the element
        // type is checked before the data is read as that type, and the length
        // comes from the same context.
        unsafe {
            if llama_cpp_sys_2::gguf_get_kv_type(self.0, id) != llama_cpp_sys_2::GGUF_TYPE_ARRAY {
                return None;
            }
            let n = llama_cpp_sys_2::gguf_get_arr_n(self.0, id);
            let data = llama_cpp_sys_2::gguf_get_arr_data(self.0, id);
            if n == 0 || data.is_null() {
                return None;
            }
            match llama_cpp_sys_2::gguf_get_arr_type(self.0, id) {
                llama_cpp_sys_2::GGUF_TYPE_UINT32 => Some(
                    std::slice::from_raw_parts(data.cast::<u32>(), n)
                        .iter()
                        .copied()
                        .map(u64::from)
                        .collect(),
                ),
                llama_cpp_sys_2::GGUF_TYPE_INT32 => Some(
                    std::slice::from_raw_parts(data.cast::<i32>(), n)
                        .iter()
                        .map(|v| u64::try_from(*v).unwrap_or(0))
                        .collect(),
                ),
                _ => None,
            }
        }
    }

    /// A key that holds one flag per layer, such as which blocks use a sliding
    /// window.
    fn bools(&self, key: &str) -> Option<Vec<bool>> {
        let id = self.key_id(key)?;
        // SAFETY: as `integers`, with the element type checked before the read.
        unsafe {
            if llama_cpp_sys_2::gguf_get_kv_type(self.0, id) != llama_cpp_sys_2::GGUF_TYPE_ARRAY
                || llama_cpp_sys_2::gguf_get_arr_type(self.0, id) != llama_cpp_sys_2::GGUF_TYPE_BOOL
            {
                return None;
            }
            let n = llama_cpp_sys_2::gguf_get_arr_n(self.0, id);
            let data = llama_cpp_sys_2::gguf_get_arr_data(self.0, id);
            if n == 0 || data.is_null() {
                return None;
            }
            Some(std::slice::from_raw_parts(data.cast::<u8>(), n).iter().map(|v| *v != 0).collect())
        }
    }
}

impl Drop for GgufMeta {
    fn drop(&mut self) {
        // SAFETY: the context came from `gguf_init_from_file` and is freed once.
        unsafe { llama_cpp_sys_2::gguf_free(self.0) };
    }
}

/// Bytes as gigabytes, for the one line a human reads.
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(kind: GgmlDeviceKind, free: u64, total: u64) -> GgmlDevice {
        GgmlDevice {
            name: "test".into(),
            description: "test".into(),
            kind,
            free_bytes: free,
            total_bytes: total,
        }
    }

    #[test]
    fn a_dedicated_card_is_believed_in_full() {
        let d = device(GgmlDeviceKind::Gpu, 6 << 30, 8 << 30);
        assert_eq!(budget_bytes(&d), 6 << 30);
    }

    #[test]
    fn shared_memory_is_bounded_by_the_host() {
        // The failure this stops: an integrated device offering more memory
        // than the machine has to spare. The host bound is whatever this test
        // machine can spare, so the assertion is that the device figure did
        // not win outright.
        let d = device(GgmlDeviceKind::IGpu, u64::MAX / 2, 8 << 30);
        assert!(budget_bytes(&d) < u64::MAX / 2, "an iGPU must not be trusted on its own");
    }

    #[test]
    fn a_driver_that_reports_its_whole_heap_is_bounded_too() {
        // free == total means the budget query was unavailable, so the figure
        // is the size of the device rather than what is unused.
        let d = device(GgmlDeviceKind::Gpu, u64::MAX / 2, u64::MAX / 2);
        assert!(budget_bytes(&d) < u64::MAX / 2);
    }

    fn shape(blocks: Vec<KvBlock>) -> ModelShape {
        ModelShape { blocks, weights_bytes: 1 }
    }

    fn block(n_head_kv: u64, dim: i64) -> KvBlock {
        KvBlock { n_head_kv, k_dim: dim, v_dim: dim, recurrent: false }
    }

    fn recurrent_block() -> KvBlock {
        KvBlock { recurrent: true, ..block(8, 512) }
    }

    #[test]
    fn kv_cost_counts_quantization_overhead() {
        let shape = shape(vec![block(1, 32), block(1, 32)]);
        // One 32-element q8_0 row is 32 bytes plus a 2-byte scale, against 64
        // bytes at f16 — so the quantized cache is smaller, but not by half.
        let f16 = shape.kv_bytes(1, KvCacheType::F16, KvCacheType::F16);
        let q8 = shape.kv_bytes(1, KvCacheType::Q8_0, KvCacheType::Q8_0);
        assert_eq!(f16, 2 * 2 * 64);
        assert_eq!(q8, 2 * 2 * 34);
    }

    #[test]
    fn kv_cost_scales_with_context() {
        let shape = shape(vec![block(2, 64); 4]);
        let one = shape.kv_bytes(1, KvCacheType::F16, KvCacheType::F16);
        assert_eq!(shape.kv_bytes(1000, KvCacheType::F16, KvCacheType::F16), one * 1000);
    }

    #[test]
    fn kv_cost_costs_each_block_on_its_own_terms() {
        // The shape of the shipped Gemma: 25 sliding-window blocks keeping eight
        // 256-wide heads, and 5 full-attention blocks keeping two 512-wide. At
        // f16 that is exactly the 220.0 KB a token measured for this model
        // (2026-08-11) — averaging the blocks instead would be out by 2×.
        let mut blocks = vec![block(8, 256); 25];
        blocks.extend(vec![block(2, 512); 5]);
        let per_token = shape(blocks).kv_bytes(1, KvCacheType::F16, KvCacheType::F16);
        assert_eq!(per_token, 225_280, "220.0 KB a token");
    }

    #[test]
    fn recurrent_blocks_cost_nothing_per_token() {
        // The shape of Qwen3.6-35B: an attention block every fourth, and the
        // rest recurrent. Ten attention blocks of two 256-wide heads is the
        // 20 KB a token measured as that model's marginal cost (2026-08-11);
        // costing all forty would claim 80.
        let mut blocks = Vec::new();
        for il in 0..40 {
            blocks.push(if (il + 1) % 4 == 0 { block(2, 256) } else { recurrent_block() });
        }
        let per_token = shape(blocks).kv_bytes(1, KvCacheType::F16, KvCacheType::F16);
        assert_eq!(per_token, 20_480, "20 KB a token");
    }

    /// What this machine decides for a real model, printed rather than asserted.
    ///
    /// The unit tests above prove the arithmetic; only a real GGUF and a real
    /// driver can show whether the *inputs* are right, and neither is available
    /// in CI. Cross-check the per-token cache figure against the one measured for
    /// the same model — 220.0 KB a token at `f16` and 116.9 KB at `q8_0` on
    /// gemma-4-26B-asym (2026-08-11) — because a wrong head count or head width
    /// still produces plausible-looking totals.
    ///
    /// ```text
    /// FONO_TEST_GGUF=/path/to/model.gguf nice -n 10 cargo test \
    ///   -p fono-core -p fono-polish --features fono-polish/accel-vulkan \
    ///   --lib gpu_offload -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a model via FONO_TEST_GGUF and a real device"]
    fn sizes_a_real_model_on_this_machine() {
        let path = std::env::var("FONO_TEST_GGUF").expect("FONO_TEST_GGUF");
        let path = Path::new(&path);
        for d in devices() {
            println!(
                "device {} ({:?}): {} free of {}, trustworthy={}",
                d.description,
                d.kind,
                gb(d.free_bytes),
                gb(d.total_bytes),
                d.free_is_trustworthy()
            );
        }
        let shape = ModelShape::probe(path).expect("the model's shape");
        println!("weights {} over {} blocks:", gb(shape.weights_bytes), shape.n_layer());
        for (il, b) in shape.blocks.iter().enumerate() {
            let kind = if b.recurrent { "recurrent" } else { "attention" };
            println!(
                "  block {il} ({kind}): {} kv heads, k={} v={}",
                b.n_head_kv, b.k_dim, b.v_dim
            );
        }
        for n_ctx in [4096u32, 32768] {
            let q8 = shape.kv_bytes(n_ctx, KvCacheType::Q8_0, KvCacheType::Q8_0);
            let f16 = shape.kv_bytes(n_ctx, KvCacheType::F16, KvCacheType::F16);
            println!(
                "ctx {n_ctx}: cache {} at q8_0 ({} bytes a token), {} at f16 ({} bytes a token)",
                gb(q8),
                q8 / u64::from(n_ctx),
                gb(f16),
                f16 / u64::from(n_ctx)
            );
            println!("  {}", decide(path, n_ctx, KvCacheType::Q8_0, KvCacheType::Q8_0).explanation);
        }
        // Then actually load it the way a role does, which is the only way to
        // learn whether the device agrees with the estimate. A refusal is not a
        // failure of this test — it exercises the host fallback — so the
        // assertion is only that some copy of the model came back.
        let model = crate::llama_backend::shared_model_sized(
            path,
            4096,
            KvCacheType::Q8_0,
            KvCacheType::Q8_0,
        )
        .expect("the model loads somewhere");
        println!("loaded: {} embedding dimensions", model.n_embd());
    }
}
