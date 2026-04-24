Nix flake
=========

Build the `fono` binary from a Nix-enabled system:

    nix build github:NimbleX/fono#default

Or, from a local checkout:

    cd packaging/nix
    nix build ../..#default    # treats the repo root as the flake input

A development shell is also exposed:

    nix develop

which drops you into a shell with `cargo`, `rustc`, `clippy`,
`rust-analyzer`, and the native build-inputs (alsa, gtk3,
libayatana-appindicator, xdotool) already on `PKG_CONFIG_PATH`.
