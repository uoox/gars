use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gars_core::{StepOutcome, Tool, ToolContext, ToolSpec};
use gars_memory::GarsPaths;
use gars_osctl::{
    InputAction, Keychain, adb_devices, adb_swipe, adb_tap, adb_text, adb_ui, input_act,
};
use serde_json::{Value, json};

use crate::arg_str;

pub struct AdbUiTool;

#[async_trait]
impl Tool for AdbUiTool {
    fn name(&self) -> &'static str {
        "adb_ui"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Dump the connected Android device's UI tree, optionally filtered by keyword.",
            json!({
                "type": "object",
                "properties": {
                    "serial": {"type": "string"},
                    "keyword": {"type": "string"},
                    "clickable_only": {"type": "boolean"}
                }
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let serial = arg_str(&args, "serial");
        let keyword = arg_str(&args, "keyword");
        let clickable_only = args
            .get("clickable_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let nodes = adb_ui(serial.as_deref(), keyword.as_deref(), clickable_only).await?;
        Ok(StepOutcome::next(
            json!({"status": "success", "nodes": nodes}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct AdbTapTool;

#[async_trait]
impl Tool for AdbTapTool {
    fn name(&self) -> &'static str {
        "adb_tap"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Tap (x, y) on an Android device via adb.",
            json!({
                "type": "object",
                "properties": {
                    "serial": {"type": "string"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"}
                },
                "required": ["x", "y"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let x = args
            .get("x")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("x required"))? as i32;
        let y = args
            .get("y")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("y required"))? as i32;
        let serial = arg_str(&args, "serial");
        let out = adb_tap(serial.as_deref(), x, y).await?;
        Ok(StepOutcome::next(
            json!({"status": "success", "output": out}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct AdbSwipeTool;

#[async_trait]
impl Tool for AdbSwipeTool {
    fn name(&self) -> &'static str {
        "adb_swipe"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Swipe from (x1, y1) to (x2, y2) over `ms` milliseconds.",
            json!({
                "type": "object",
                "properties": {
                    "serial": {"type": "string"},
                    "x1": {"type": "integer"},
                    "y1": {"type": "integer"},
                    "x2": {"type": "integer"},
                    "y2": {"type": "integer"},
                    "ms": {"type": "integer", "default": 300}
                },
                "required": ["x1", "y1", "x2", "y2"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let extract = |key: &str| -> Result<i32> {
            args.get(key)
                .and_then(Value::as_i64)
                .map(|v| v as i32)
                .ok_or_else(|| anyhow!("{key} required"))
        };
        let out = adb_swipe(
            arg_str(&args, "serial").as_deref(),
            extract("x1")?,
            extract("y1")?,
            extract("x2")?,
            extract("y2")?,
            args.get("ms").and_then(Value::as_i64).unwrap_or(300) as i32,
        )
        .await?;
        Ok(StepOutcome::next(
            json!({"status": "success", "output": out}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct AdbTextTool;

#[async_trait]
impl Tool for AdbTextTool {
    fn name(&self) -> &'static str {
        "adb_text"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Type text into the focused Android input.",
            json!({
                "type": "object",
                "properties": {
                    "serial": {"type": "string"},
                    "text": {"type": "string"}
                },
                "required": ["text"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let text = arg_str(&args, "text").ok_or_else(|| anyhow!("text required"))?;
        let out = adb_text(arg_str(&args, "serial").as_deref(), &text).await?;
        Ok(StepOutcome::next(
            json!({"status": "success", "output": out}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct AdbDevicesTool;

#[async_trait]
impl Tool for AdbDevicesTool {
    fn name(&self) -> &'static str {
        "adb_devices"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "List connected Android devices via adb.",
            json!({"type": "object", "properties": {}}),
        )
    }

    async fn execute(&self, _args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let devices = adb_devices().await?;
        Ok(StepOutcome::next(
            json!({"status": "success", "devices": devices}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct KeychainSetTool;

#[async_trait]
impl Tool for KeychainSetTool {
    fn name(&self) -> &'static str {
        "keychain_set"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Store a secret in ~/.gars/keychain.enc (XOR-encrypted local file). Either pass `value` or `file`.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "value": {"type": "string"},
                    "file": {"type": "string"}
                },
                "required": ["name"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let name = arg_str(&args, "name").ok_or_else(|| anyhow!("name required"))?;
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let mut kc = Keychain::open(paths.home.join("keychain.enc"))?;
        if let Some(value) = arg_str(&args, "value") {
            kc.set(&name, value.as_bytes())?;
        } else if let Some(file) = arg_str(&args, "file") {
            kc.set_from_file(&name, &ctx.resolve_path(&file))?;
        } else {
            return Err(anyhow!("value or file required"));
        }
        Ok(StepOutcome::next(
            json!({"status": "success", "stored": name}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct KeychainUseTool;

#[async_trait]
impl Tool for KeychainUseTool {
    fn name(&self) -> &'static str {
        "keychain_use"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Retrieve a keychain entry as base64 bytes (or utf8 text if `as_text=true`). The returned content is sensitive.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "as_text": {"type": "boolean", "default": true}
                },
                "required": ["name"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let name = arg_str(&args, "name").ok_or_else(|| anyhow!("name required"))?;
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let kc = Keychain::open(paths.home.join("keychain.enc"))?;
        let value = kc.get(&name)?;
        let as_text = args.get("as_text").and_then(Value::as_bool).unwrap_or(true);
        let payload = if as_text {
            json!({"text": String::from_utf8_lossy(&value).into_owned()})
        } else {
            use base64::Engine;
            use base64::engine::general_purpose::STANDARD as B64;
            json!({"bytes_base64": B64.encode(&value)})
        };
        Ok(StepOutcome::next(
            json!({"status": "success", "payload": payload}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct KeychainListTool;

#[async_trait]
impl Tool for KeychainListTool {
    fn name(&self) -> &'static str {
        "keychain_list"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "List keychain entries with masked previews.",
            json!({"type": "object", "properties": {}}),
        )
    }

    async fn execute(&self, _args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let kc = Keychain::open(paths.home.join("keychain.enc"))?;
        let entries = kc.list();
        Ok(StepOutcome::next(
            json!({"status": "success", "entries": entries}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct InputActTool;

#[async_trait]
impl Tool for InputActTool {
    fn name(&self) -> &'static str {
        "input_act"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Drive the local keyboard/mouse. Set dry_run=true to plan without firing. Supported actions: click, move, type, key, screenshot.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["click", "move", "type", "key", "screenshot"]},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "button": {"type": "string"},
                    "text": {"type": "string"},
                    "seq": {"type": "string"},
                    "bbox": {"type": "array", "items": {"type": "integer"}},
                    "out_path": {"type": "string"},
                    "dry_run": {"type": "boolean"}
                },
                "required": ["action"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let action: InputAction = serde_json::from_value(args)?;
        let result = input_act(action).await?;
        Ok(StepOutcome::next(
            json!({
                "status": result.status,
                "backend": result.backend,
                "message": result.message,
            }),
            ctx.anchor_prompt(),
        ))
    }
}
