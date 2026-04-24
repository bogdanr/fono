// SPDX-License-Identifier: GPL-3.0-only
//! `TextFormatter` trait — cleanup a raw STT string into polished text.

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct FormatContext {
    pub main_prompt: String,
    pub advanced_prompt: String,
    pub dictionary: Vec<String>,
    pub rule_suffix: Option<String>,
    pub app_class: Option<String>,
    pub app_title: Option<String>,
    pub language: Option<String>,
}

impl FormatContext {
    /// Build the system prompt to send to the LLM.
    #[must_use]
    pub fn system_prompt(&self) -> String {
        let mut s = String::new();
        if !self.main_prompt.is_empty() {
            s.push_str(&self.main_prompt);
            s.push_str("\n\n");
        }
        if !self.advanced_prompt.is_empty() {
            s.push_str(&self.advanced_prompt);
            s.push_str("\n\n");
        }
        if !self.dictionary.is_empty() {
            s.push_str("Personal dictionary (preserve spelling exactly): ");
            s.push_str(&self.dictionary.join(", "));
            s.push_str("\n\n");
        }
        if let Some(sfx) = &self.rule_suffix {
            s.push_str(sfx);
            s.push_str("\n\n");
        }
        s.trim_end().to_string()
    }
}

#[async_trait]
pub trait TextFormatter: Send + Sync {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String>;
    fn name(&self) -> &'static str;
}
