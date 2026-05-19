// SPDX-License-Identifier: GPL-3.0-only
//! Probe: print the filtered STT/polish backend list for the loaded secrets.
//! Used to verify the "configured backends" logic without running the daemon.

use fono_core::{
    config::{PolishBackend, SttBackend},
    providers, Secrets,
};

fn main() {
    let path =
        std::env::args().nth(1).unwrap_or_else(|| "/root/.config/fono/secrets.toml".to_string());
    let secrets = Secrets::load(std::path::Path::new(&path)).expect("load secrets");
    println!("secrets file : {path}");
    println!("keys present : {:?}", secrets.keys.keys().collect::<Vec<_>>());
    let stt = providers::configured_stt_backends(&secrets, &SttBackend::Groq);
    let polish = providers::configured_polish_backends(&secrets, &PolishBackend::Groq);
    println!("STT visible  : {stt:?}");
    println!("Polish visible  : {polish:?}");
}
