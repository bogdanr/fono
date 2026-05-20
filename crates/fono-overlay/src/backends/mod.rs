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

pub mod noop;

#[cfg(feature = "backend-x11")]
pub mod winit_x11;

#[cfg(feature = "backend-wlr")]
pub mod wayland_layer_shell;

#[cfg(feature = "backend-wlr")]
pub(crate) mod wayland_shm;
