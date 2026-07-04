// SPDX-License-Identifier: GPL-3.0-only
//! Hotkey registration probe — a diagnostic, not part of the binary.
//!
//! Creates a `GlobalHotKeyManager` on the calling thread, registers the
//! given keys (default: `F7`, `F8`, `Escape`), reports each result, then
//! unregisters and exits. Useful for answering "can this session grab
//! global hotkeys at all?" without starting the daemon — e.g. over
//! headless SSH on macOS, where Carbon `RegisterEventHotKey` behaviour
//! outside a WindowServer session is the question itself (macOS port
//! plan, Phase 5 headless gate).
//!
//! Usage: `cargo run -p fono-hotkey --example probe [key ...]`

use global_hotkey::GlobalHotKeyManager;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let keys: Vec<String> =
        if args.is_empty() { vec!["F7".into(), "F8".into(), "Escape".into()] } else { args };

    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => {
            println!("manager: OK");
            m
        }
        Err(e) => {
            println!("manager: FAILED — {e}");
            std::process::exit(1);
        }
    };

    let mut failures = 0;
    for key in &keys {
        let hk = match fono_hotkey::parse_hotkey(key) {
            Ok(p) => p.into_hotkey(),
            Err(e) => {
                println!("parse {key}: FAILED — {e:#}");
                failures += 1;
                continue;
            }
        };
        match manager.register(hk) {
            Ok(()) => {
                println!("register {key}: OK");
                match manager.unregister(hk) {
                    Ok(()) => println!("unregister {key}: OK"),
                    Err(e) => {
                        println!("unregister {key}: FAILED — {e}");
                        failures += 1;
                    }
                }
            }
            Err(e) => {
                println!("register {key}: FAILED — {e}");
                failures += 1;
            }
        }
    }
    std::process::exit(i32::from(failures > 0));
}
