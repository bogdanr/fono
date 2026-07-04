// SPDX-License-Identifier: GPL-3.0-only
//! macOS TCC permission probes (Accessibility).
//!
//! CGEvent posting silently drops events when the process lacks the
//! Accessibility grant, so callers must *probe* rather than inject and
//! hope (macOS port plan Task 9.3). Two entry points:
//!
//! - [`accessibility_trusted`] — silent check, safe to call anywhere
//!   (doctor, periodic status).
//! - [`accessibility_prompt`] — the guided first-run path: asks the OS
//!   to raise its native "fono would like to control this computer"
//!   dialog, which deep-links the user straight to the right Settings
//!   pane. macOS shows that dialog at most once per app identity;
//!   later calls behave like the silent check.
//!
//! On non-macOS targets both return `None` (the concept doesn't
//! exist), keeping call sites free of `cfg` noise.
//!
//! Zero new crates: raw FFI onto ApplicationServices/CoreFoundation,
//! which the binary already links via CoreAudio/AppKit.

/// Silent Accessibility-trust check. `None` off macOS.
#[must_use]
pub fn accessibility_trusted() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: AXIsProcessTrusted takes no arguments and only reads
        // the calling process's TCC state.
        Some(unsafe { macos::AXIsProcessTrusted() })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Accessibility-trust check that asks the OS to show its native
/// permission dialog (with a deep link to the Settings pane) when the
/// grant is missing. Returns the current trust state; `None` off
/// macOS. The dialog only appears in a graphical session — headless
/// this degrades to the silent check.
#[must_use]
pub fn accessibility_prompt() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        Some(macos::trusted_with_prompt())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// `open`-able deep link to the Accessibility pane in System Settings.
pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        /// CFStringRef key: "AXTrustedCheckOptionPrompt".
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFBooleanTrue: *const c_void;
        // Opaque callback structs — only their addresses are passed.
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    /// `AXIsProcessTrustedWithOptions({prompt: true})`, releasing the
    /// options dictionary afterwards. Falls back to the silent probe if
    /// the dictionary can't be built.
    pub fn trusted_with_prompt() -> bool {
        // SAFETY: keys/values arrays outlive the CFDictionaryCreate
        // call, which copies them; the returned dictionary is released
        // exactly once; the AX call only reads it.
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            );
            if options.is_null() {
                return AXIsProcessTrusted();
            }
            let trusted = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            trusted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_probe_matches_platform() {
        let t = accessibility_trusted();
        if cfg!(target_os = "macos") {
            // Headless SSH or CI: either state is legal, but the probe
            // must answer without crashing.
            assert!(t.is_some());
        } else {
            assert!(t.is_none());
        }
    }

    #[test]
    fn settings_url_is_the_accessibility_pane() {
        assert!(ACCESSIBILITY_SETTINGS_URL.contains("Privacy_Accessibility"));
    }
}
