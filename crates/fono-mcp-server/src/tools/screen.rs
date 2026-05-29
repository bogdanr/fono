// SPDX-License-Identifier: GPL-3.0-only
//! `fono.screen` — capture a screenshot and return it as an MCP image block.

use async_trait::async_trait;
use base64::Engine as _;
use fono_core::screen_capture::{CaptureError, CaptureMode, CaptureSource, GrabberProbe};
use fono_inject::focus::detect_focus;
use fono_ipc::McpPhase;

use crate::protocol::ToolCallResult;
use crate::tools::{McpContext, Tool};
use crate::voice_io::McpActivityGuard;

/// `fono.screen` MCP tool.
///
/// Captures a screenshot (automatic or interactive) and returns it as an MCP
/// image content block (`{"type":"image","data":"<base64>","mimeType":"image/png"}`).
pub struct ScreenTool {
    probe: GrabberProbe,
    daemon_ipc_candidates: Vec<std::path::PathBuf>,
}

impl ScreenTool {
    pub fn new(ctx: &McpContext) -> Self {
        Self {
            probe: GrabberProbe::detect(),
            daemon_ipc_candidates: ctx.daemon_ipc_candidates.clone(),
        }
    }
}

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str {
        "fono.screen"
    }

    fn description(&self) -> &str {
        "Capture a screenshot. mode=automatic grabs the focused window instantly (use \
         for 'look at this error', 'what window is this'). mode=interactive opens a \
         region picker so the user can frame a specific area (use when the user says \
         'let me show you a piece of this' or 'this part here'). Only call this when \
         the user explicitly references something visible on their screen."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["mode"],
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["automatic", "interactive"],
                    "description": "automatic = grab focused window instantly; interactive = let user select region"
                }
            }
        })
    }

    async fn call(&self, arguments: serde_json::Value) -> ToolCallResult {
        let mode_str = match arguments.get("mode").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => return ToolCallResult::failure("fono.screen: missing `mode` argument"),
        };
        let mode = match mode_str {
            "automatic" => CaptureMode::Automatic,
            "interactive" => CaptureMode::Interactive,
            other => {
                return ToolCallResult::failure(format!("fono.screen: unknown mode `{other}`"))
            }
        };

        // Flash tray amber while we capture.
        let _guard = McpActivityGuard::new(McpPhase::Speaking, &self.daemon_ipc_candidates);

        // Get focused window class for the privacy gate (automatic mode only).
        let focused_wm_class: Option<String> = if mode == CaptureMode::Automatic {
            detect_focus().ok().and_then(|f| f.window_class)
        } else {
            None
        };

        // Clone what we need to move into the blocking task.
        let probe = self.probe.clone();
        let mode_str_owned = mode_str.to_string();

        match tokio::task::spawn_blocking(move || {
            let wm_class_ref = focused_wm_class.as_deref();
            probe.capture(mode, wm_class_ref)
        })
        .await
        {
            Ok(Ok(img)) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&img.png_bytes);
                let source_label = match &img.source {
                    CaptureSource::Window { wm_class, .. } => wm_class.clone(),
                    CaptureSource::Region => "region".to_string(),
                };
                let meta = serde_json::json!({
                    "source": source_label,
                    "dimensions": format!("{}x{}", img.width, img.height),
                    "tool": img.tool,
                    "mode": mode_str_owned,
                });
                // Phase 6 fast-path removed — vision model reads the screenshot directly.
                let content_blocks = vec![
                    crate::protocol::ContentBlock::image(b64, "image/png".to_string()),
                    crate::protocol::ContentBlock::text(meta.to_string()),
                ];
                crate::protocol::ToolCallResult { content: content_blocks, is_error: false }
            }
            Ok(Err(CaptureError::PrivateWindow)) => ToolCallResult::failure(
                "Cannot capture: focused window is private (password manager or similar).",
            ),
            Ok(Err(CaptureError::Cancelled)) => ToolCallResult::success("{\"cancelled\": true}"),
            Ok(Err(CaptureError::NoToolAvailable)) => ToolCallResult::failure(
                "No screen capture tool available. \
                 Install scrot (X11) or grim+slurp (Wayland).",
            ),
            Ok(Err(e)) => ToolCallResult::failure(format!("fono.screen: {e}")),
            Err(e) => ToolCallResult::failure(format!("fono.screen: task error: {e}")),
        }
    }
}
