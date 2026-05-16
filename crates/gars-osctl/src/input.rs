//! Physical mouse/keyboard input. We avoid adding `enigo` as a hard dependency
//! because its Linux X11/Wayland glue brings non-trivial system requirements;
//! instead we shell out to platform-appropriate helpers and degrade gracefully
//! when none are available. Tools can always specify `dry_run = true` to plan
//! without actually firing input events.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputAction {
    Click {
        x: i32,
        y: i32,
        #[serde(default = "default_button")]
        button: String,
        #[serde(default)]
        dry_run: bool,
    },
    Move {
        x: i32,
        y: i32,
        #[serde(default)]
        dry_run: bool,
    },
    Type {
        text: String,
        #[serde(default)]
        dry_run: bool,
    },
    Key {
        seq: String,
        #[serde(default)]
        dry_run: bool,
    },
    Screenshot {
        #[serde(default)]
        bbox: Option<[i32; 4]>,
        out_path: String,
    },
}

fn default_button() -> String {
    "left".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputResult {
    pub status: String,
    pub backend: String,
    pub message: String,
}

pub async fn input_act(action: InputAction) -> Result<InputResult> {
    let os = std::env::consts::OS;
    match action {
        InputAction::Click {
            x,
            y,
            button,
            dry_run,
        } => {
            if dry_run {
                return Ok(InputResult {
                    status: "dry_run".into(),
                    backend: "noop".into(),
                    message: format!("would click {button} at ({x},{y})"),
                });
            }
            click(os, x, y, &button).await
        }
        InputAction::Move { x, y, dry_run } => {
            if dry_run {
                return Ok(InputResult {
                    status: "dry_run".into(),
                    backend: "noop".into(),
                    message: format!("would move to ({x},{y})"),
                });
            }
            move_to(os, x, y).await
        }
        InputAction::Type { text, dry_run } => {
            if dry_run {
                return Ok(InputResult {
                    status: "dry_run".into(),
                    backend: "noop".into(),
                    message: format!("would type {} chars", text.chars().count()),
                });
            }
            type_text(os, &text).await
        }
        InputAction::Key { seq, dry_run } => {
            if dry_run {
                return Ok(InputResult {
                    status: "dry_run".into(),
                    backend: "noop".into(),
                    message: format!("would send key {seq}"),
                });
            }
            send_key(os, &seq).await
        }
        InputAction::Screenshot { bbox, out_path } => screenshot(os, bbox, &out_path).await,
    }
}

async fn click(os: &str, x: i32, y: i32, button: &str) -> Result<InputResult> {
    match os {
        "macos" => {
            let script = format!(
                "do shell script \"echo 'click {x},{y}'\"\ntell application \"System Events\"\n  click at {{{}, {}}}\nend tell",
                x, y
            );
            let _ = button; // macOS osascript click is left-only
            run("osascript", &["-e", &script])
                .await
                .map(|out| InputResult {
                    status: "ok".into(),
                    backend: "osascript".into(),
                    message: out,
                })
        }
        "linux" => run(
            "xdotool",
            &[
                "mousemove",
                &x.to_string(),
                &y.to_string(),
                "click",
                &button_code(button).to_string(),
            ],
        )
        .await
        .map(|out| InputResult {
            status: "ok".into(),
            backend: "xdotool".into(),
            message: out,
        }),
        _ => Err(anyhow!("unsupported OS: {os}")),
    }
}

fn button_code(b: &str) -> u8 {
    match b {
        "right" => 3,
        "middle" => 2,
        _ => 1,
    }
}

async fn move_to(os: &str, x: i32, y: i32) -> Result<InputResult> {
    match os {
        "linux" => run("xdotool", &["mousemove", &x.to_string(), &y.to_string()])
            .await
            .map(|out| InputResult {
                status: "ok".into(),
                backend: "xdotool".into(),
                message: out,
            }),
        "macos" => Ok(InputResult {
            status: "skipped".into(),
            backend: "macos".into(),
            message: format!("macOS lacks pure-move primitive; use click directly at ({x},{y})"),
        }),
        _ => Err(anyhow!("move not supported on {os}")),
    }
}

async fn type_text(os: &str, text: &str) -> Result<InputResult> {
    match os {
        "linux" => run("xdotool", &["type", "--", text])
            .await
            .map(|out| InputResult {
                status: "ok".into(),
                backend: "xdotool".into(),
                message: out,
            }),
        "macos" => {
            let script = format!(
                "tell application \"System Events\" to keystroke \"{}\"",
                text.replace('"', "\\\"")
            );
            run("osascript", &["-e", &script])
                .await
                .map(|out| InputResult {
                    status: "ok".into(),
                    backend: "osascript".into(),
                    message: out,
                })
        }
        _ => Err(anyhow!("type not supported on {os}")),
    }
}

async fn send_key(os: &str, seq: &str) -> Result<InputResult> {
    match os {
        "linux" => run("xdotool", &["key", seq]).await.map(|out| InputResult {
            status: "ok".into(),
            backend: "xdotool".into(),
            message: out,
        }),
        "macos" => Ok(InputResult {
            status: "skipped".into(),
            backend: "macos".into(),
            message: format!("macOS key sequences need explicit osascript mapping for {seq}"),
        }),
        _ => Err(anyhow!("send_key not supported on {os}")),
    }
}

async fn screenshot(os: &str, bbox: Option<[i32; 4]>, out_path: &str) -> Result<InputResult> {
    match os {
        "macos" => {
            let mut args = Vec::new();
            args.push("-x".to_string());
            if let Some([x, y, w, h]) = bbox {
                args.push("-R".into());
                args.push(format!("{x},{y},{w},{h}"));
            }
            args.push(out_path.to_string());
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run("screencapture", &arg_refs)
                .await
                .map(|out| InputResult {
                    status: "ok".into(),
                    backend: "screencapture".into(),
                    message: out,
                })
        }
        "linux" => {
            let arg = if let Some([x, y, w, h]) = bbox {
                vec![
                    "-window".to_string(),
                    "root".into(),
                    "-crop".into(),
                    format!("{w}x{h}+{x}+{y}"),
                    out_path.into(),
                ]
            } else {
                vec!["-window".to_string(), "root".into(), out_path.into()]
            };
            let arg_refs: Vec<&str> = arg.iter().map(String::as_str).collect();
            run("import", &arg_refs).await.map(|out| InputResult {
                status: "ok".into(),
                backend: "imagemagick".into(),
                message: out,
            })
        }
        _ => Err(anyhow!("screenshot not supported on {os}")),
    }
}

async fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| anyhow!("spawn {cmd} failed: {err}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
