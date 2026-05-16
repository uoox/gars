use std::process::Stdio;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
}

pub async fn adb_devices() -> Result<Vec<AdbDevice>> {
    let out = adb(&[], None, &["devices"]).await?;
    let mut out_devs = Vec::new();
    for line in out.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let serial = parts.next().unwrap_or("").to_string();
        let state = parts.next().unwrap_or("unknown").to_string();
        if !serial.is_empty() {
            out_devs.push(AdbDevice { serial, state });
        }
    }
    Ok(out_devs)
}

pub async fn adb_tap(serial: Option<&str>, x: i32, y: i32) -> Result<String> {
    let xs = x.to_string();
    let ys = y.to_string();
    adb(&["shell"], serial, &["input", "tap", &xs, &ys]).await
}

pub async fn adb_swipe(
    serial: Option<&str>,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    ms: i32,
) -> Result<String> {
    let x1s = x1.to_string();
    let y1s = y1.to_string();
    let x2s = x2.to_string();
    let y2s = y2.to_string();
    let mss = ms.to_string();
    adb(
        &["shell"],
        serial,
        &["input", "swipe", &x1s, &y1s, &x2s, &y2s, &mss],
    )
    .await
}

pub async fn adb_text(serial: Option<&str>, text: &str) -> Result<String> {
    let escaped = text.replace(' ', "%s").replace('\n', "%n");
    adb(&["shell"], serial, &["input", "text", &escaped]).await
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdbNode {
    pub text: String,
    pub resource_id: String,
    pub class: String,
    pub package: String,
    pub content_desc: String,
    pub bounds: String,
    pub clickable: bool,
}

pub async fn adb_ui(
    serial: Option<&str>,
    keyword: Option<&str>,
    clickable_only: bool,
) -> Result<Vec<AdbNode>> {
    adb(
        &["shell"],
        serial,
        &["uiautomator", "dump", "/sdcard/ui.xml"],
    )
    .await?;
    let xml = adb(&["shell"], serial, &["cat", "/sdcard/ui.xml"]).await?;
    parse_ui_dump(&xml, keyword, clickable_only)
}

fn parse_ui_dump(xml: &str, keyword: Option<&str>, clickable_only: bool) -> Result<Vec<AdbNode>> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let kw_lower = keyword.map(|k| k.to_lowercase());
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                if e.name().as_ref() != b"node" {
                    continue;
                }
                let mut node = AdbNode {
                    text: String::new(),
                    resource_id: String::new(),
                    class: String::new(),
                    package: String::new(),
                    content_desc: String::new(),
                    bounds: String::new(),
                    clickable: false,
                };
                for attr in e.attributes().flatten() {
                    let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
                    let val = attr
                        .unescape_value()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    match key {
                        "text" => node.text = val,
                        "resource-id" => node.resource_id = val,
                        "class" => node.class = val,
                        "package" => node.package = val,
                        "content-desc" => node.content_desc = val,
                        "bounds" => node.bounds = val,
                        "clickable" => node.clickable = val == "true",
                        _ => {}
                    }
                }
                if clickable_only && !node.clickable {
                    continue;
                }
                if let Some(kw) = &kw_lower {
                    let combined =
                        format!("{} {} {}", node.text, node.resource_id, node.content_desc)
                            .to_lowercase();
                    if !combined.contains(kw) {
                        continue;
                    }
                }
                out.push(node);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("xml error at {}: {e}", reader.buffer_position())),
            _ => {}
        }
    }
    Ok(out)
}

async fn adb(prefix: &[&str], serial: Option<&str>, command: &[&str]) -> Result<String> {
    let mut cmd = Command::new("adb");
    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }
    cmd.args(prefix).args(command);
    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| anyhow!("adb spawn failed (is adb on PATH?): {err}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "adb {:?} failed: {}",
            command,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_node() {
        let xml = r#"<hierarchy>
<node text="hello" resource-id="" class="android.widget.TextView" package="x" content-desc="" bounds="[0,0][10,10]" clickable="false"/>
</hierarchy>"#;
        let n = parse_ui_dump(xml, None, false).unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].text, "hello");
    }
}
