// SPDX-License-Identifier: GPL-3.0-only
// Placeholder binary for the crates.io stub package.
//
// End users never run this. The real application is distributed as a prebuilt
// binary via GitHub Releases (and fetched by `cargo binstall fono`). This stub
// exists only so the `fono` name and per-version binstall metadata live on
// crates.io.
fn main() {
    eprintln!(
        "This crates.io package only provides `cargo binstall` metadata.\n\
         Install the app with:  cargo binstall fono\n\
         Or use the one-liner:  curl -fsSL https://fono.page/install | sh\n\
         Releases: https://github.com/bogdanr/fono/releases"
    );
}
