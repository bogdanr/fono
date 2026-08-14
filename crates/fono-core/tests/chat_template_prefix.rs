// SPDX-License-Identifier: GPL-3.0-only
//! Is llama.cpp's own chat-template rendering append-only?
//!
//! The prompt-state cache pins a rendered prefix and reuses it next turn, so
//! adopting `llama_chat_apply_template` is only worth it if rendering messages
//! `0..n` is a prefix of rendering `0..n+1`. These walk every template
//! llama.cpp names and assert it, so an upstream change cannot quietly turn
//! every turn back into a full prefill.
//!
//! No model is needed: `llama_chat_apply_template` takes the template as text.

#![cfg(feature = "llama-local")]

use std::ffi::{c_char, CString};

const NAMES: &[&str] = &[
    "chatml",
    "llama2",
    "llama2-sys",
    "llama2-sys-bos",
    "llama2-sys-strip",
    "mistral-v1",
    "mistral-v3",
    "mistral-v3-tekken",
    "mistral-v7",
    "mistral-v7-tekken",
    "phi3",
    "phi4",
    "falcon3",
    "zephyr",
    "monarch",
    "gemma",
    "orion",
    "openchat",
    "vicuna",
    "vicuna-orca",
    "deepseek",
    "deepseek2",
    "deepseek3",
    "deepseek-ocr",
    "command-r",
    "llama3",
    "chatglm3",
    "chatglm4",
    "glmedge",
    "minicpm",
    "exaone3",
    "exaone4",
    "exaone-moe",
    "rwkv-world",
    "granite",
    "granite-4.0",
    "granite-4.1",
    "gigachat",
    "megrez",
    "yandex",
    "bailing",
    "bailing-think",
    "bailing2",
    "llama4",
    "smolvlm",
    "hunyuan-moe",
    "gpt-oss",
    "hunyuan-dense",
    "hunyuan-vl",
    "kimi-k2",
    "seed_oss",
    "grok-2",
];

fn render(tmpl: &str, msgs: &[(&str, &str)], add_ass: bool) -> Option<String> {
    let tmpl = CString::new(tmpl).unwrap();
    let owned: Vec<(CString, CString)> =
        msgs.iter().map(|(r, c)| (CString::new(*r).unwrap(), CString::new(*c).unwrap())).collect();
    let raw: Vec<llama_cpp_sys_2::llama_chat_message> = owned
        .iter()
        .map(|(r, c)| llama_cpp_sys_2::llama_chat_message { role: r.as_ptr(), content: c.as_ptr() })
        .collect();
    let mut buf = vec![0u8; 8192];
    let res = unsafe {
        llama_cpp_sys_2::llama_chat_apply_template(
            tmpl.as_ptr(),
            raw.as_ptr(),
            raw.len(),
            add_ass,
            buf.as_mut_ptr().cast::<c_char>(),
            buf.len() as i32,
        )
    };
    if res < 0 {
        return None;
    }
    buf.truncate(res as usize);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Rendering a conversation's history must be append-only, or a pinned prefix
/// can never be reused: the prompt-state cache restores the deepest cached
/// entry that is a token-prefix of the new prompt, so a template that rewrites
/// earlier turns costs every turn a full prefill.
///
/// Matching happens on tokens at run time, so a template that failed this
/// would lose cache hits rather than serve a wrong prompt. Asserting it here
/// keeps that from degrading silently.
#[test]
fn rendering_history_is_append_only_for_every_template() {
    let full = conversation();
    let mut broken = Vec::new();

    for name in NAMES {
        let renders: Vec<String> = [2usize, 4, 6]
            .iter()
            .map(|end| {
                render(name, &full[..*end], false)
                    .unwrap_or_else(|| panic!("{name} is not a template llama.cpp knows"))
            })
            .collect();
        if !(renders[1].starts_with(&renders[0]) && renders[2].starts_with(&renders[1])) {
            broken.push((*name, renders));
        }
    }

    for (name, renders) in &broken {
        report_divergence(name, &renders[0], &renders[1]);
    }
    assert!(broken.is_empty(), "history rendering rewrites earlier turns for some templates");
}

/// Two templates end the prompt with a generation cue — yandex with `[SEP]`,
/// bailing-think with `<think>` — which the reply then replaces, so asking for
/// the assistant header is *not* append-only. Anything pinned in the cache must
/// therefore stop at the end of the history and leave the cue to the suffix.
#[test]
fn a_generation_cue_is_not_part_of_the_history() {
    let full = conversation();
    let mut cue_rewritten = Vec::new();

    for name in NAMES {
        let renders: Vec<String> = [2usize, 4, 6]
            .iter()
            .map(|end| render(name, &full[..*end], true).expect("known template"))
            .collect();
        if !(renders[1].starts_with(&renders[0]) && renders[2].starts_with(&renders[1])) {
            cue_rewritten.push(*name);
        }
    }

    assert_eq!(
        cue_rewritten,
        vec!["yandex", "bailing-think"],
        "the set of templates whose trailing cue is overwritten by the reply has changed"
    );
}

fn conversation() -> Vec<(&'static str, &'static str)> {
    vec![
        ("system", "You are terse."),
        ("user", "First question."),
        ("assistant", "First answer."),
        ("user", "Second question."),
        ("assistant", "Second answer."),
        ("user", "Third question."),
    ]
}

fn report_divergence(name: &str, short: &str, long: &str) {
    let at = short.bytes().zip(long.bytes()).take_while(|(x, y)| x == y).count();
    let window = |s: &str| {
        let lo = at.saturating_sub(40);
        let hi = s.len().min(at + 40);
        String::from_utf8_lossy(&s.as_bytes()[lo..hi]).into_owned()
    };
    println!("--- {name}: diverges at byte {at} of {}", short.len());
    println!("    short: {:?}", window(short));
    println!("    long : {:?}", window(long));
}
