# Patch: strip llama.cpp `common/` from `llama-cpp-sys-2 0.1.145`

Prepared for Phase 1 Task 1.1 of
`plans/2026-04-30-fono-single-binary-size-v1.md`.

This patch adds an opt-out `common` cargo feature to `llama-cpp-sys-2`.
With the feature off, the crate skips:

- the `LLAMA_BUILD_COMMON=ON` cmake define (so `libcommon.a` is never
  produced by the upstream `llama.cpp/CMakeLists.txt:199-200`
  `add_subdirectory(common)` block);
- the `wrapper_common.cpp` + `wrapper_oai.cpp` C++ build that produces
  `libllama_cpp_sys_2_common_wrapper.a` (`build.rs:499-522`);
- the `cargo:rustc-link-lib=static=common` emit (`build.rs:1014`).

Default behaviour preserved: `default = ["common"]` keeps every existing
downstream user on the current code path. Fono opts out via
`default-features = false` in `Cargo.toml`.

Estimated saving on the Fono binary after LTO + `--gc-sections`:
**6–10 MB of `.text`** (24 MB of static archives drop out of the link).

## Verified preconditions

`crates/fono-llm/src/llama_local.rs:29-60` references only:

- `llama_cpp_2::context::params::LlamaContextParams`
- `llama_cpp_2::llama_backend::LlamaBackend`
- `llama_cpp_2::llama_batch::LlamaBatch`
- `llama_cpp_2::model::params::LlamaModelParams`
- `llama_cpp_2::model::{AddBos, LlamaModel}`
- `llama_cpp_2::sampling::LlamaSampler`
- `llama_cpp_2::send_logs_to_tracing`
- `llama_cpp_2::LogOptions`

All of these resolve through `llama.h` (the core API in
`llama.cpp/include/llama.h`); none touch `llama.cpp/common/*`.

`llama-cpp-2 0.1.145` (the high-level binding) does not reference
`common/` either — the wrapper headers `wrapper_common.h` /
`wrapper_oai.h` are only consumed by the sys crate's optional helpers
that fono never calls.

## Diff (apply against
`llama-cpp-sys-2 0.1.145` `Cargo.toml.orig` + `build.rs`)

```diff
--- a/Cargo.toml.orig
+++ b/Cargo.toml.orig
@@ -80,6 +80,9 @@ name = "llama-cpp-sys-2"
 [features]
+# Build llama.cpp's `common/` helper library (CLI arg parsing, sampling
+# helpers, server bits). Default-on for backwards compatibility.
+# Opt out via `default-features = false` to drop ~24 MB of native code.
+common = []
 cuda = []
 cuda-no-vmm = ["cuda"]
 dynamic-link = []
@@ -95,6 +98,9 @@ system-ggml = []
 system-ggml-static = ["system-ggml"]
 vulkan = []
+
+[features.default]
+default = ["common"]
```

(Note: the actual upstream `Cargo.toml.orig` uses `[package]` ordering;
the maintainer can place the `default = ["common"]` line wherever the
existing default block lives. If no default block exists today, add
`default = ["common"]` at the end of the `[features]` table.)

```diff
--- a/build.rs
+++ b/build.rs
@@ -496,6 +496,8 @@ fn main() -> Result<(), Box<dyn Error>> {
     println!("cargo:rerun-if-changed=wrapper_common.h");
     println!("cargo:rerun-if-changed=wrapper_common.cpp");
     println!("cargo:rerun-if-changed=wrapper_oai.h");
@@ -494,6 +496,7 @@
     println!("cargo:rerun-if-changed=wrapper_oai.cpp");
     println!("cargo:rerun-if-changed=wrapper_utils.h");
     println!("cargo:rerun-if-changed=wrapper_mtmd.h");

     debug_log!("Bindings Created");

+    if cfg!(feature = "common") {
     let mut common_wrapper_build = cc::Build::new();
     common_wrapper_build
         .cpp(true)
@@ -519,6 +522,7 @@
         common_wrapper_build.cpp_link_stdlib(None);
     }

     common_wrapper_build.compile("llama_cpp_sys_2_common_wrapper");
+    }

     // Build with Cmake

@@ -532,7 +536,7 @@
     config.define("LLAMA_BUILD_TESTS", "OFF");
     config.define("LLAMA_BUILD_EXAMPLES", "OFF");
     config.define("LLAMA_BUILD_SERVER", "OFF");
     config.define("LLAMA_BUILD_TOOLS", "OFF");
-    config.define("LLAMA_BUILD_COMMON", "ON");
+    config.define("LLAMA_BUILD_COMMON", if cfg!(feature = "common") { "ON" } else { "OFF" });
     config.define("LLAMA_CURL", "OFF");

@@ -1000,6 +1004,7 @@
     assert_ne!(llama_libs.len(), 0);

+    if cfg!(feature = "common") {
     let common_lib_dir = out_dir.join("build").join("common");
     if common_lib_dir.is_dir() {
         println!(
@@ -1015,6 +1020,7 @@
         }
         println!("cargo:rustc-link-lib=static=common");
     }
+    }

     if cfg!(feature = "system-ggml") {
```

## How to apply (two paths)

### Option A — vendored fork in our repo (`+22 MB git, fully self-contained`)

```sh
mkdir -p vendor
cp -a ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/llama-cpp-sys-2-0.1.145 \
      vendor/llama-cpp-sys-2
cd vendor/llama-cpp-sys-2
patch -p1 < ../../plans/2026-04-30-llama-cpp-sys-2-strip-common.patch.md
```

Then add to workspace `Cargo.toml`:

```toml
[patch.crates-io]
llama-cpp-sys-2 = { path = "vendor/llama-cpp-sys-2" }
```

And add to `crates/fono-llm/Cargo.toml`:

```toml
llama-cpp-2 = { workspace = true, default-features = false }
```

(The `default-features = false` removes the implicit `common` feature
on the parent crate; `llama-cpp-2 0.1.145` does not have its own
`common` feature so this only removes the propagation to the sys
crate.)

### Option B — git fork on GitHub

1. Fork `https://github.com/utilityai/llama-cpp-rs` to your account.
2. Apply the patch above on a `fono-strip-common` branch.
3. Push.
4. Add to workspace `Cargo.toml`:

```toml
[patch.crates-io]
llama-cpp-sys-2 = { git = "https://github.com/<you>/llama-cpp-rs",
                    branch = "fono-strip-common" }
```

Same `default-features = false` change in `crates/fono-llm/Cargo.toml`.

## Upstream contribution

Either option ships a binary today; in parallel, file an upstream PR at
`https://github.com/utilityai/llama-cpp-rs` adding the same `common`
feature gate. Once accepted and a release is cut, drop our `[patch]`
override.

## Verification once landed

```sh
# After build, the libcommon archives must not exist:
test ! -f target/x86_64-unknown-linux-musl/release-slim/build/llama-cpp-sys-2-*/out/build/common/libcommon.a
# And the wrapper archive must not exist:
test ! -f target/x86_64-unknown-linux-musl/release-slim/build/llama-cpp-sys-2-*/out/libllama_cpp_sys_2_common_wrapper.a
# Smoke test still passes:
cargo test -p fono --test local_backends_coexist
```
