#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fono Windows port: drive the remote Windows build sandbox from the Linux
# side.
#
# Task 0.8 of plans/2026-05-26-windows-port-v1.md. The dev Windows box is
# reached over OpenSSH (its default remote shell is cmd.exe, not a POSIX
# shell) and its identity is deliberately kept out of the repo: the host
# comes exclusively from FONO_WIN_HOST — an ssh destination or ssh_config
# alias, e.g. `user@host` — set in your shell or an untracked local file
# you source. There is no default and there must never be one.
#
# Prereqs on the Windows box (see docs/build-windows.md for the full
# walkthrough and the gotchas the design plan didn't call out):
#   - OpenSSH Server + key auth (password auth disabled)
#   - Visual Studio Build Tools 2022, "Desktop development with C++"
#     workload (MSVC v143, Windows SDK, CMake, Ninja)
#   - The VS-bundled CMake's bin dir added to the *system* PATH — it is
#     not on PATH by default outside a "Native Tools" prompt, which a
#     plain SSH session never is.
#   - A standalone LLVM/Clang install with LIBCLANG_PATH set system-wide
#     (Build Tools does NOT bundle libclang.dll; bindgen, used by
#     llama-cpp-sys-2 and whisper-rs-sys, needs it).
#   - `LongPathsEnabled=1` under
#     HKLM\SYSTEM\CurrentControlSet\Control\FileSystem, plus
#     `git config --global core.longpaths true` — the vendored
#     llama.cpp submodule checkout exceeds Windows' legacy 260-char
#     MAX_PATH.
#   - Rust via rustup (x86_64-pc-windows-msvc host), clippy + rustfmt
#     components.
#   - rsync reachable over SSH (MSYS2 `pacman -S rsync openssh`, or a
#     recent Git for Windows).
#
# `push` mirrors the local working tree (tracked + untracked-but-not-
# ignored files, per .gitignore; minus .git/) into C:\fono-dev\fono with
# --delete, so the remote always matches what you see locally — including
# uncommitted edits. `check`/`build`/`test`/`cargo` also resolve
# ORT_LIB_LOCATION by running scripts/fetch-onnxruntime.sh remotely (via
# MSYS bash, which has curl/xz/sha256sum on PATH unlike a bare cmd.exe
# session) — cheap and idempotent: it no-ops once the pinned
# onnxruntime.lib is cached under the remote target/ dir.
#
# Usage:
#   scripts/win-remote.sh push               # rsync working tree -> Windows
#   scripts/win-remote.sh check              # push + cargo check --workspace
#   scripts/win-remote.sh build [args…]      # push + cargo build [args…]
#   scripts/win-remote.sh test  [args…]      # push + cargo test --workspace --tests --lib [args…]
#   scripts/win-remote.sh cargo <args…>      # push + arbitrary cargo command
#   scripts/win-remote.sh sh <command…>      # raw cmd.exe in the repo dir, no push

set -eu

usage() {
	cat <<'EOF'
usage: scripts/win-remote.sh <command> [args…]   (requires FONO_WIN_HOST)

  push               rsync working tree -> Windows sandbox
  check              push + cargo check --workspace
  build [args…]      push + cargo build [args…]
  test  [args…]      push + cargo test --workspace --tests --lib [args…]
  cargo <args…>      push + arbitrary cargo command
  sh <command…>      raw cmd.exe in the repo dir, no push
EOF
}

[ "${1:-}" ] || {
	usage >&2
	exit 2
}

if [ -z "${FONO_WIN_HOST:-}" ]; then
	echo "error: FONO_WIN_HOST is not set." >&2
	echo "Set it to the Windows box's ssh destination (user@host or an" >&2
	echo "ssh_config alias). It is intentionally not stored anywhere in" >&2
	echo "the repo." >&2
	exit 2
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
remote_win_dir='C:\fono-dev\fono'
remote_rsync_dir='/c/fono-dev/fono'
ort_dir='C:\fono-dev\fono\target\onnxruntime-1.24.2\x86_64-pc-windows-msvc'

# On MSVC there is no `stdc++.lib`. `.cargo/config.toml` sets
# ORT_CXX_STDLIB=static:-bundle=stdc++ for the Linux-gnu NEEDED allowlist,
# but cargo's [env] table is not target-scoped, so ort-sys would emit a
# bogus `-lstdc++` here and the final link fails with LNK1181. An *empty*
# ORT_CXX_STDLIB makes ort-sys fall back to its correct MSVC default (no
# explicit C++ stdlib link — the MSVC CRT is linked automatically). cmd.exe
# cannot hold an empty-valued env var, so we override the config value with
# an empty TOML string via `--config`. Single quotes (a TOML literal empty
# string) survive cmd.exe unmangled where `""` would not. Windows port
# Task 3.3 / docs/build-windows.md.
#
# The same target-scoping gap applies to the GNU/Clang size flags
# CFLAGS/CXXFLAGS in `.cargo/config.toml` (`-Os -ffunction-sections …`): MSVC
# `cl` rejects the `-f*` flags and on a CLEAN checkout that breaks
# ggml-vulkan's `vulkan-shaders-gen` CMake compiler probe. Blank them the same
# way so `cl` uses its own defaults.
win_neutralise="--config env.ORT_CXX_STDLIB='' --config env.CFLAGS='' --config env.CXXFLAGS=''"

push() {
	# Same exclude/filter policy as scripts/mac-remote.sh: honour every
	# .gitignore in the tree (so local bulk never crosses the wire) while
	# explicitly protecting /target from --delete (it holds the pinned
	# onnxruntime.lib and hours of MSBuild/llama.cpp cache).
	rsync -av --delete \
		--exclude '/target' \
		--filter ':- .gitignore' \
		--exclude '/.git/' \
		--exclude '*~' \
		"$repo_root"/ "$FONO_WIN_HOST:$remote_rsync_dir/"
}

fetch_ort() {
	# shellcheck disable=SC2029 # remote-side expansion is intentional
	ssh "$FONO_WIN_HOST" \
		"C:\\msys64\\usr\\bin\\bash.exe -lc \"cd /c/fono-dev/fono && TARGET=x86_64-pc-windows-msvc sh scripts/fetch-onnxruntime.sh\"" \
		>/dev/null
}

run_remote() {
	# cmd.exe gotcha: `set VAR=value && next` with a *space* before `&&`
	# bakes the trailing space into the value (cmd.exe `set` has no
	# implicit trim). Always quote as `set "VAR=value"` to avoid it.
	# shellcheck disable=SC2029 # remote-side expansion is intentional
	ssh "$FONO_WIN_HOST" "set \"ORT_LIB_LOCATION=$ort_dir\" && cd $remote_win_dir && $*"
}

cmd=$1
shift
case "$cmd" in
push) push ;;
check) push && fetch_ort && run_remote cargo check $win_neutralise --workspace "$@" ;;
build) push && fetch_ort && run_remote cargo build $win_neutralise "$@" ;;
test) push && fetch_ort && run_remote cargo test $win_neutralise --workspace --tests --lib "$@" ;;
cargo) push && fetch_ort && run_remote cargo $win_neutralise "$@" ;;
sh) run_remote "$@" ;;
*)
	usage >&2
	exit 2
	;;
esac
