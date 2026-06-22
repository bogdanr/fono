# Installing Fono

Fono is a single static binary (~22 MB) with four glibc dependencies.
Threre is also a GPU accelerated version (~60 MB); macOS and Windows 
are planned.

There are three ways to install:
 - the one-liner script
 - the distro packages attached to releases
 - manual download + `sudo fono install`.

## One-liner (recommended)

```sh
curl -fsSL https://fono.page/install | sh
```

The script (published at <https://fono.page/install>; source in
`packaging/install.sh`) detects your CPU and probes for Vulkan, downloads
the matching prebuilt binary into a temp dir, then runs `sudo fono
install` in auto-detect mode. The self-installer places the binary on
`$PATH`, writes the desktop entry / autostart hook / systemd unit (as
appropriate for the host), starts the daemon, and opens the `fono setup`
wizard in the same terminal.

Useful environment knobs:

| Variable | Effect |
|---|---|
| `FONO_VERSION=vX.Y.Z` | pin to a specific release tag |
| `FONO_VARIANT=cpu\|gpu` | override variant detection |
| `FONO_MODE=desktop\|server` | override mode detection |
| `FONO_INSTALL_NO_START=1` | skip the post-install daemon launch |
| `BIN_DIR=/path` | legacy: bypass `fono install` and drop the binary in `BIN_DIR`

## Manual install

Download the archive that matches your CPU + GPU + libc from the
[latest release](https://github.com/bogdanr/fono/releases/latest), extract
it, and run:

```sh
sudo ./fono install
```

`sudo fono install` (with no flags) auto-detects whether the host has a
graphical session and picks the right lane:

- A visible X11 or Wayland session, an active `loginctl Type=x11/wayland`
  session, a known display-manager unit, or a writable `/tmp/.X11-unix`
  socket triggers the **desktop** lane.
- An empty graphical surface and `systemctl get-default` returning
  `multi-user.target` (or no systemd at all) triggers the **server**
  lane with a one-line banner naming the trigger.
- Anything ambiguous defaults to desktop.

Force a lane explicitly when needed:

```sh
sudo fono install --desktop      # binary + menu entry + autostart + icon + completions
sudo fono install --server       # binary + systemd unit running as user `fono` + completions
sudo fono install --dry-run      # print the planned actions, write nothing
```

## Server mode (Wyoming STT host)

`sudo fono install --server` is the way to run Fono headless as a LAN
STT server that Home Assistant, Rhasspy, or other Fono clients can
auto-discover via mDNS and route transcription through.

> Prefer Docker? See [home-assistant.md](home-assistant.md) for the
> prebuilt multi-arch Wyoming STT/TTS container.

The installer seeds `/etc/fono/config.toml` (only when no config exists
yet) with the Wyoming listener already enabled on `0.0.0.0:10300`:

```toml
[server.wyoming]
enabled = true
bind = "0.0.0.0"
port = 10300
```

It chowns the file `root:fono 0640`, starts the systemd unit, and
post-start probes `127.0.0.1:10300` over TCP to confirm the listener
actually bound. The install summary prints the bound address, a
security caveat, and a hint to install a Whisper model if
`/var/lib/fono/models/` is empty.

**Security.** Wyoming v1 has no in-band authentication. Binding to
`0.0.0.0` exposes inference to every host that can route to TCP/10300.
Restrict exposure by:

- changing `bind` to `"127.0.0.1"` for loopback-only,
- changing `bind` to a specific NIC address (e.g. `"192.168.1.5"`),
- or blocking port 10300 at your firewall (iptables / nftables / ufw /
  firewalld).

After any edit: `sudo systemctl restart fono.service`.

Re-running `sudo fono install --server` is idempotent — an existing
`/etc/fono/config.toml` is preserved byte-for-byte.

The server lane requires an STT backend to actually transcribe. With
the default `[stt].backend = "local"` you need at least one Whisper
model under `/var/lib/fono/models/`:

```sh
sudo -u fono fono models install small             # 182 MB, multilingual
sudo -u fono fono models install large-v3-turbo    # 834 MB, strongest local
```

## Distro packages

`.deb`, `.pkg.tar.zst`, and `.txz` files are built by CI and attached to
each [release](https://github.com/bogdanr/fono/releases/latest). They
are **not regularly tested** — they may work; please file an issue if
they don't.

## Updating

```sh
fono update                 # check for a newer release, prompt, replace in place
fono update --check         # only check; exits 0 if up-to-date, 1 if not
fono update --channel prerelease    # follow pre-releases
```

The daemon also checks automatically in the background when
`[update].auto_check` is enabled (the default). `fono update`
re-execs into the new binary after replacing it, so no manual restart
is needed.

## Uninstalling

```sh
sudo fono uninstall           # reverse a previous `fono install`
sudo fono uninstall --dry-run # preview only
```

Removes every system path the installer wrote (binary, desktop entries,
icon, systemd unit, completions). In desktop mode it also wipes
`~/.cache/fono` (model weights and downloaded archives — fully
reproducible on next `fono setup`). In server mode it wipes
`/var/cache/fono`.

User data under `~/.config/fono`, `~/.local/share/fono`, and
`~/.local/state/fono` is **never touched** — remove those by hand if
you want a clean slate. See [privacy.md](privacy.md) for the full data
inventory.

## Platforms

- **Linux x86_64 and aarch64** — first-class. Daily-driven by the
  maintainer on NimbleX / Slackware; CI also covers Ubuntu and Arch.
- **macOS and Windows** — planned, not shipping. See
  [ROADMAP.md](../ROADMAP.md).

## Verification

After install, `fono doctor` prints a diagnostic report covering config,
paths, providers, audio device, injector, overlay backend, hotkey
backend, and tray host. If something looks off, paste the output into
a [troubleshooting recipe](troubleshooting.md) or a GitHub issue.
