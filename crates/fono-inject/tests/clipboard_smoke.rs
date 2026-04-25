// SPDX-License-Identifier: GPL-3.0-only
//! Smoke test for the clipboard fallback. Skipped in CI; run on a host
//! with at least one clipboard tool (xsel/xclip/wl-copy) available.

#[test]
#[ignore = "requires xsel/xclip/wl-copy on PATH"]
fn copy_to_clipboard_does_not_hang() {
    use std::time::{Duration, Instant};
    let started = Instant::now();
    let result = fono_inject::copy_to_clipboard("fono test payload xyzzy-1234");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "copy_to_clipboard should return in <2s, took {elapsed:?}"
    );
    assert!(
        result.is_ok(),
        "copy_to_clipboard returned error: {:?}",
        result.err()
    );
}
