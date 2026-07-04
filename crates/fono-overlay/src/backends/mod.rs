// SPDX-License-Identifier: GPL-3.0-only
//! Backend implementations. Each module behind its own cargo feature.
//!
//! - [`noop`] — always compiled. Terminal fallback so spawn never
//!   aborts on hosts without a graphics environment.
//! - [`winit_x11`] — `backend-x11` feature. Carryover of the
//!   `winit` + `softbuffer` path. X11-only after the 2026-05-19
//!   winit Wayland strip; also reached on Wayland sessions via
//!   Xwayland (the GNOME / KDE-Wayland default).
//! - [`wayland_layer_shell`] — `backend-wlr` feature. Primary
//!   Wayland path on every compositor that implements
//!   `zwlr_layer_shell_v1` (sway, hyprland, KDE Wayland, COSMIC, …).
//! - [`macos`] — `backend-macos` feature. Native NSPanel path on
//!   macOS, blitting the shared software renderer via the AppKit
//!   main-thread pump installed by `fono::main`.

pub mod noop;

// The Linux graphical backends are display-server stacks; their
// crates are only declared for `cfg(target_os = "linux")` (see
// Cargo.toml), so the modules are gated on feature AND target.

#[cfg(all(feature = "backend-x11", target_os = "linux"))]
pub mod winit_x11;

#[cfg(all(feature = "backend-wlr", target_os = "linux"))]
pub mod wayland_layer_shell;

#[cfg(all(feature = "backend-wlr", target_os = "linux"))]
pub(crate) mod wayland_shm;

#[cfg(all(feature = "backend-macos", target_os = "macos"))]
pub mod macos;
