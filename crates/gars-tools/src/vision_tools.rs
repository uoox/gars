use std::path::PathBuf;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gars_core::{StepOutcome, Tool, ToolContext, ToolSpec};
use gars_vision::{VisionConfig, ocr_image, vision_describe};
use serde_json::{Value, json};

use crate::arg_str;

#[derive(Clone, Debug, Default)]
pub struct VisionToolOptions {
    pub config: VisionConfig,
}

pub struct ImageDescribeTool {
    pub config: VisionConfig,
}

#[async_trait]
impl Tool for ImageDescribeTool {
    fn name(&self) -> &'static str {
        "image_describe"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Send an image to a vision LLM with a specific prompt. Crop first; never blindly send full screen.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "prompt": {"type": "string"}
                },
                "required": ["path", "prompt"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let path =
            ctx.resolve_path(&arg_str(&args, "path").ok_or_else(|| anyhow!("path required"))?);
        let prompt = arg_str(&args, "prompt").ok_or_else(|| anyhow!("prompt required"))?;
        let text = vision_describe(&self.config, &path, &prompt).await?;
        Ok(StepOutcome::next(
            json!({"status": "success", "text": text}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct OcrImageTool {
    pub config: VisionConfig,
}

#[async_trait]
impl Tool for OcrImageTool {
    fn name(&self) -> &'static str {
        "ocr_image"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Run local OCR on an image via the ocrs CLI (install with `pip install ocrs-cli` if missing). Returns text + lines.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let path =
            ctx.resolve_path(&arg_str(&args, "path").ok_or_else(|| anyhow!("path required"))?);
        let result = ocr_image(&self.config, &path).await?;
        Ok(StepOutcome::next(
            json!({
                "status": "success",
                "text": result.text,
                "lines": result.lines,
                "source": result.source,
            }),
            ctx.anchor_prompt(),
        ))
    }
}

fn _ignore() -> PathBuf {
    PathBuf::new()
}
