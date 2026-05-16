use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, anyhow};
use clap::{Parser, ValueEnum};
use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};
use gars_llm::parse_root_config;
use gars_memory::{GarsPaths, default_config, legacy_home, migration_hint};
use gars_osctl::Keychain;
use reqwest::header::AUTHORIZATION;
use serde_json::Value;
use uuid::Uuid;

/// macOS LaunchAgent label. v0.10 renamed from `cc.uoox.gars` to a proper
/// reverse-DNS string matching the GitHub source URL. Linux uses
/// `gars.service` instead, so these consts are mac-only.
#[cfg(target_os = "macos")]
const LAUNCH_LABEL: &str = "com.github.uoox.gars";
/// v0.9 and earlier wrote a plist under this name; we delete it on install
/// / uninstall so the upgrade is clean.
#[cfg(target_os = "macos")]
const LEGACY_LAUNCH_LABEL: &str = "cc.uoox.gars";

#[derive(Parser)]
#[command(
    name = "garstool",
    version,
    about = "Interactive manager for the gars service"
)]
struct Cli {
    #[arg(long, env = "GARS_HOME")]
    home: Option<PathBuf>,
    #[arg(long)]
    yes: bool,
    #[arg(value_enum)]
    action: Option<Action>,
}

#[derive(Clone, Debug, ValueEnum)]
enum Action {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
    Logs,
    Configure,
    /// Wipe ~/.gars (backed up to ~/.gars.backup.<ts>/) + uninstall service +
    /// rerun configure + reinstall + start. Useful for a clean reset without
    /// reinstalling the binary itself.
    Reset,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = GarsPaths::resolve(cli.home)?;
    if let Some(hint) = migration_hint(&paths) {
        println!("{hint}");
    }
    paths.ensure()?;
    match cli.action {
        Some(action) => run_action(&paths, action, cli.yes).await,
        None => interactive_menu(&paths).await,
    }
}

async fn interactive_menu(paths: &GarsPaths) -> Result<()> {
    loop {
        let items = [
            "安装服务",
            "卸载服务",
            "启动服务",
            "停止服务",
            "重启服务",
            "查看状态",
            "配置向导",
            "查看日志",
            "全新安装（清空并重装）",
            "退出",
        ];
        let choice = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("gars 管理菜单")
            .items(items)
            .default(5)
            .interact()?;
        match choice {
            0 => run_action(paths, Action::Install, false).await?,
            1 => run_action(paths, Action::Uninstall, false).await?,
            2 => run_action(paths, Action::Start, false).await?,
            3 => run_action(paths, Action::Stop, false).await?,
            4 => run_action(paths, Action::Restart, false).await?,
            5 => run_action(paths, Action::Status, true).await?,
            6 => run_action(paths, Action::Configure, false).await?,
            7 => run_action(paths, Action::Logs, true).await?,
            8 => run_action(paths, Action::Reset, false).await?,
            _ => break,
        }
    }
    Ok(())
}

async fn run_action(paths: &GarsPaths, action: Action, yes: bool) -> Result<()> {
    match action {
        Action::Install => install_service(paths, yes),
        Action::Uninstall => uninstall_service(paths, yes),
        Action::Start => start_service(paths),
        Action::Stop => stop_service(paths),
        Action::Restart => {
            let _ = stop_service(paths);
            start_service(paths)
        }
        Action::Status => status(paths).await,
        Action::Logs => show_logs(paths),
        Action::Configure if yes => {
            if !paths.config.exists() {
                fs::write(&paths.config, default_config())?;
                println!("已写入默认配置 {}", paths.config.display());
            } else {
                println!("配置已存在 {}", paths.config.display());
            }
            Ok(())
        }
        Action::Configure => configure(paths),
        Action::Reset => reset(paths, yes).await,
    }
}

/// 全新安装：保存现有 ~/.gars 为 .backup.<ts>/、卸载服务、重新走配置向导、
/// 重新安装服务、启动。**会把现有运行态全部清掉**，所以默认要二次确认。
async fn reset(paths: &GarsPaths, yes: bool) -> Result<()> {
    if !yes {
        println!();
        println!("\x1b[1;33m⚠  全新安装会做以下事情：\x1b[0m");
        println!("  1. 停止 + 卸载当前服务");
        println!("  2. 把 ~/.gars 整个备份到 ~/.gars.backup.<时间戳>/ （不会删 backup）");
        println!("  3. 重新走配置向导（重置 LLM / 端口 / token）");
        println!("  4. 重新安装服务并启动");
        println!("  备份目录保留，可手动恢复或删除。");
        if !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("继续？")
            .default(false)
            .interact()?
        {
            return Ok(());
        }
    }

    println!();
    println!("→ [1/4] 停止并卸载现有服务");
    let _ = stop_service(paths);
    let _ = uninstall_service(paths, true);

    println!();
    println!("→ [2/4] 备份 {}", paths.home.display());
    if paths.home.exists() {
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let backup = paths.home.with_extension(format!("backup.{stamp}"));
        fs::rename(&paths.home, &backup).map_err(|e| {
            anyhow!(
                "重命名 {} → {} 失败: {e}",
                paths.home.display(),
                backup.display()
            )
        })?;
        println!("  ✓ 已备份到 {}", backup.display());
    } else {
        println!("  (没有 ~/.gars，跳过备份)");
    }
    paths.ensure()?;
    println!("  ✓ 重建空白 ~/.gars/");

    println!();
    println!("→ [3/4] 运行配置向导");
    configure(paths)?;

    println!();
    println!("→ [4/4] 安装服务并启动");
    install_service(paths, true)?;
    let _ = start_service(paths);

    println!();
    println!("✓ 全新安装完成。用「查看状态」检查 /health。");
    Ok(())
}

struct LlmPreset {
    label: &'static str,
    provider: &'static str,
    api_base: &'static str,
    model: &'static str,
    api_key_env: &'static str,
    fake_cc: bool,
    user_agent: Option<&'static str>,
    thinking_type: Option<&'static str>,
    note: &'static str,
}

const LLM_PRESETS: &[LlmPreset] = &[
    LlmPreset {
        label: "OpenAI (gpt-5.2)",
        provider: "openai_compatible",
        api_base: "https://api.openai.com/v1/chat/completions",
        model: "gpt-5.2",
        api_key_env: "OPENAI_API_KEY",
        fake_cc: false,
        user_agent: None,
        thinking_type: None,
        note: "OpenAI native; sk-proj-* or sk-* key in OPENAI_API_KEY.",
    },
    LlmPreset {
        label: "Claude (Anthropic 直连)",
        provider: "anthropic",
        api_base: "https://api.anthropic.com",
        model: "claude-sonnet-4-6",
        api_key_env: "ANTHROPIC_API_KEY",
        fake_cc: false,
        user_agent: None,
        thinking_type: Some("adaptive"),
        note: "sk-ant-* key. fake_cc_system_prompt OFF.",
    },
    LlmPreset {
        label: "Claude (CC switch / Claude Code 透传)",
        provider: "anthropic",
        api_base: "https://YOUR-CC-SWITCH-HOST/claude/office",
        model: "claude-opus-4-7",
        api_key_env: "CC_RELAY_KEY",
        fake_cc: true,
        user_agent: Some("claude-cli/2.1.113 (external, cli)"),
        thinking_type: Some("adaptive"),
        note: "sk-user-* / sk-* / cr_* key. fake_cc_system_prompt ON.",
    },
    LlmPreset {
        label: "Claude (CRS relay, cr_* key)",
        provider: "anthropic",
        api_base: "https://YOUR-CRS-HOST/api",
        model: "claude-opus-4-7[1m]",
        api_key_env: "CRS_CLAUDE_KEY",
        fake_cc: true,
        user_agent: Some("claude-cli/2.1.113 (external, cli)"),
        thinking_type: Some("adaptive"),
        note: "cr_* key + [1m] 触发 1M context beta.",
    },
    LlmPreset {
        label: "智谱 GLM-5.1 (Anthropic 兼容)",
        provider: "anthropic",
        api_base: "https://open.bigmodel.cn/api/anthropic",
        model: "glm-5.1",
        api_key_env: "ZHIPU_API_KEY",
        fake_cc: false,
        user_agent: None,
        thinking_type: None,
        note: "智谱平台的 Anthropic 兼容接口.",
    },
    LlmPreset {
        label: "MiniMax M2.7 (Anthropic 兼容)",
        provider: "anthropic",
        api_base: "https://api.minimaxi.com/anthropic",
        model: "MiniMax-M2.7",
        api_key_env: "MINIMAX_API_KEY",
        fake_cc: false,
        user_agent: None,
        thinking_type: None,
        note: "MiniMax 的 Anthropic 兼容路径 (无 <think> 标签).",
    },
    LlmPreset {
        label: "DeepSeek (V4)",
        provider: "openai_compatible",
        api_base: "https://api.deepseek.com/v1/chat/completions",
        model: "deepseek-chat",
        api_key_env: "DEEPSEEK_API_KEY",
        fake_cc: false,
        user_agent: None,
        thinking_type: None,
        note: "DeepSeek OpenAI 兼容路径.",
    },
    LlmPreset {
        label: "Custom (手动填写)",
        provider: "openai_compatible",
        api_base: "https://api.example.com/v1/chat/completions",
        model: "model-name",
        api_key_env: "API_KEY",
        fake_cc: false,
        user_agent: None,
        thinking_type: None,
        note: "自定义所有字段.",
    },
];

fn configure(paths: &GarsPaths) -> Result<()> {
    offer_legacy_migration(paths)?;
    println!();
    println!("\x1b[1;36m==>\x1b[0m 配置向导");
    println!("配置文件: {}", paths.config.display());
    if paths.config.exists() {
        println!("(已有配置 — 保存时会覆盖)");
    }
    let theme = ColorfulTheme::default();
    let language: String = Input::with_theme(&theme)
        .with_prompt("语言 language")
        .default("zh".into())
        .interact_text()?;
    let bind: String = Input::with_theme(&theme)
        .with_prompt("REST bind")
        .default("127.0.0.1".into())
        .interact_text()?;
    let port: u16 = Input::with_theme(&theme)
        .with_prompt("REST port")
        .default(9221)
        .interact_text()?;

    let preset_labels: Vec<&str> = LLM_PRESETS.iter().map(|p| p.label).collect();
    let preset_idx = Select::with_theme(&theme)
        .with_prompt("选择 LLM 预设")
        .items(&preset_labels)
        .default(0)
        .interact()?;
    let preset = &LLM_PRESETS[preset_idx];
    println!("→ {}", preset.note);

    let provider: String = Input::with_theme(&theme)
        .with_prompt("LLM provider")
        .default(preset.provider.into())
        .interact_text()?;
    let api_base: String = Input::with_theme(&theme)
        .with_prompt("LLM API base")
        .default(preset.api_base.into())
        .interact_text()?;
    let model: String = Input::with_theme(&theme)
        .with_prompt("LLM model")
        .default(preset.model.into())
        .interact_text()?;
    let api_key_env: String = Input::with_theme(&theme)
        .with_prompt("API key env var")
        .default(preset.api_key_env.into())
        .interact_text()?;
    let api_key: String = Password::with_theme(&theme)
        .with_prompt("(可选) 直接填入 api_key (留空走环境变量)")
        .allow_empty_password(true)
        .interact()?;

    let chat_placeholder: String = Input::with_theme(&theme)
        .with_prompt("聊天平台配置备注/chat connector note")
        .default("configure Telegram / Feishu connectors here later".into())
        .interact_text()?;
    let admin_token = Password::with_theme(&theme)
        .with_prompt("REST admin token (empty to generate)")
        .allow_empty_password(true)
        .interact()?;
    let admin_token = if admin_token.trim().is_empty() {
        Uuid::new_v4().to_string()
    } else {
        admin_token
    };

    let mut extras = String::new();
    if preset.fake_cc {
        extras.push_str("fake_cc_system_prompt = true\n");
    }
    if let Some(ua) = preset.user_agent {
        extras.push_str(&format!("user_agent = \"{ua}\"\n"));
    }
    if let Some(tt) = preset.thinking_type {
        extras.push_str(&format!("thinking_type = \"{tt}\"\n"));
    }
    let key_line = if api_key.trim().is_empty() {
        format!("api_key_env = \"{api_key_env}\"")
    } else {
        format!("api_key = \"{api_key}\"\napi_key_env = \"{api_key_env}\"")
    };

    let content = format!(
        r#"# gars configuration.
language = "{language}"
default_llm = "primary"
context_char_budget = 180000

[server]
bind = "{bind}"
port = {port}
admin_token = "{admin_token}"

[browser]
host = "127.0.0.1"
port = 9222

[connectors]
note = "{chat_placeholder}"

[[llm.sessions]]
name = "primary"
provider = "{provider}"
api_base = "{api_base}"
{key_line}
model = "{model}"
native_tools = true
stream = true
max_tokens = 16384
temperature = 1.0
{extras}
[[llm.mixins]]
name = "primary"
sessions = ["primary"]
max_retries = 3
base_delay_ms = 800
spring_back_secs = 300
"#
    );
    parse_root_config(&content)?;
    fs::write(&paths.config, content)?;
    println!("✓ 已写入 {}", paths.config.display());
    configure_advanced(paths)
}

fn configure_advanced(paths: &GarsPaths) -> Result<()> {
    let theme = ColorfulTheme::default();
    loop {
        let items = [
            "连接器 (Telegram / 飞书)",
            "凭证管理 (keychain)",
            "Skills (初始化默认 SOP / 远端搜索)",
            "归档 (auto / 间隔)",
            "立即触发归档",
            "返回",
        ];
        let choice = Select::with_theme(&theme)
            .with_prompt("高级配置子菜单")
            .items(items)
            .default(5)
            .interact()?;
        match choice {
            0 => configure_connectors(paths)?,
            1 => manage_keychain(paths)?,
            2 => configure_skills(paths)?,
            3 => configure_archive(paths)?,
            4 => trigger_archive(paths)?,
            _ => break,
        }
    }
    Ok(())
}

fn configure_connectors(paths: &GarsPaths) -> Result<()> {
    let theme = ColorfulTheme::default();
    let connectors = ["telegram", "feishu"];
    let labels = ["Telegram", "飞书/Lark"];
    let choice = Select::with_theme(&theme)
        .with_prompt("选择要配置的连接器")
        .items(labels)
        .default(0)
        .interact()?;
    let id = connectors[choice];
    let mut value = read_config_table(paths)?;
    let connectors_tbl = value
        .as_table_mut()
        .unwrap()
        .entry("connectors".to_string())
        .or_insert(toml::Value::Table(toml::Table::new()));
    let connector_tbl = connectors_tbl
        .as_table_mut()
        .unwrap()
        .entry(id.to_string())
        .or_insert(toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap();
    let enable = Confirm::with_theme(&theme)
        .with_prompt(format!("启用 {id} 连接器？"))
        .default(false)
        .interact()?;
    connector_tbl.insert("enable".to_string(), toml::Value::Boolean(enable));
    if enable {
        match id {
            "telegram" => {
                let token: String = Input::with_theme(&theme)
                    .with_prompt("Telegram bot token (留空读取 TG_BOT_TOKEN 环境变量)")
                    .allow_empty(true)
                    .interact_text()?;
                if !token.trim().is_empty() {
                    connector_tbl.insert("token".to_string(), toml::Value::String(token));
                }
                let chats: String = Input::with_theme(&theme)
                    .with_prompt("允许的 chat_id 列表（逗号分隔，留空允许全部）")
                    .allow_empty(true)
                    .interact_text()?;
                if !chats.trim().is_empty() {
                    let arr = chats
                        .split(',')
                        .map(|s| toml::Value::String(s.trim().to_string()))
                        .collect();
                    connector_tbl.insert("allow_chats".to_string(), toml::Value::Array(arr));
                }
            }
            "feishu" => {
                let app_id: String = Input::with_theme(&theme)
                    .with_prompt("Feishu app_id")
                    .interact_text()?;
                let app_secret: String = Password::with_theme(&theme)
                    .with_prompt("Feishu app_secret")
                    .interact()?;
                let encrypt_key: String = Input::with_theme(&theme)
                    .with_prompt("encrypt_key (可选)")
                    .allow_empty(true)
                    .interact_text()?;
                let verification_token: String = Input::with_theme(&theme)
                    .with_prompt("verification_token (可选)")
                    .allow_empty(true)
                    .interact_text()?;
                connector_tbl.insert("app_id".to_string(), toml::Value::String(app_id));
                connector_tbl.insert("app_secret".to_string(), toml::Value::String(app_secret));
                if !encrypt_key.is_empty() {
                    connector_tbl
                        .insert("encrypt_key".to_string(), toml::Value::String(encrypt_key));
                }
                if !verification_token.is_empty() {
                    connector_tbl.insert(
                        "verification_token".to_string(),
                        toml::Value::String(verification_token),
                    );
                }
            }
            _ => unreachable!("connector list is exhaustive: telegram + feishu"),
        }
    }
    write_config_table(paths, &value)?;
    println!("已写入 {}", paths.config.display());
    Ok(())
}

fn manage_keychain(paths: &GarsPaths) -> Result<()> {
    let theme = ColorfulTheme::default();
    let mut kc = Keychain::open(paths.home.join("keychain.enc"))?;
    loop {
        let entries = kc.list();
        let summary = if entries.is_empty() {
            "(空)".to_string()
        } else {
            entries
                .iter()
                .map(|e| format!("{} → {}", e.name, e.mask))
                .collect::<Vec<_>>()
                .join("\n  ")
        };
        println!("当前凭证：\n  {summary}");
        let items = ["列出", "新增/更新", "删除", "返回"];
        let choice = Select::with_theme(&theme)
            .with_prompt("凭证管理")
            .items(items)
            .default(3)
            .interact()?;
        match choice {
            0 => continue,
            1 => {
                let name: String = Input::with_theme(&theme)
                    .with_prompt("name")
                    .interact_text()?;
                let value = Password::with_theme(&theme)
                    .with_prompt("value")
                    .interact()?;
                kc.set(&name, value.as_bytes())?;
                println!("已写入 {name}。");
            }
            2 => {
                let name: String = Input::with_theme(&theme)
                    .with_prompt("要删除的 name")
                    .interact_text()?;
                kc.delete(&name)?;
                println!("已删除 {name}。");
            }
            _ => break,
        }
    }
    Ok(())
}

fn configure_skills(paths: &GarsPaths) -> Result<()> {
    let theme = ColorfulTheme::default();
    let summary = gars_skills::init_user_skills(paths)?;
    println!(
        "刷新内置 SOP：{} 个，内置 agent：{} 个，内置 mode：{} 个",
        summary.builtin_skills.len(),
        summary.builtin_agents.len(),
        summary.builtin_modes.len()
    );
    if !summary.migrated_to_builtin.is_empty() || !summary.migrated_to_local.is_empty() {
        println!(
            "v0.3 平铺布局已迁移：builtin {} 个，local {} 个；详情见 ~/.gars/MIGRATION_v0.4.log",
            summary.migrated_to_builtin.len(),
            summary.migrated_to_local.len()
        );
    }
    let api: String = Input::with_theme(&theme)
        .with_prompt("Skill Search API URL（留空 = 仅本地 BM25）")
        .default("http://www.fudankw.cn:58787".into())
        .allow_empty(true)
        .interact_text()?;
    let mut value = read_config_table(paths)?;
    let skills_tbl = value
        .as_table_mut()
        .unwrap()
        .entry("skills".to_string())
        .or_insert(toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap();
    skills_tbl.insert("remote".to_string(), toml::Value::String(api));
    write_config_table(paths, &value)?;
    Ok(())
}

fn configure_archive(paths: &GarsPaths) -> Result<()> {
    let theme = ColorfulTheme::default();
    let auto = Confirm::with_theme(&theme)
        .with_prompt("启用自动归档？")
        .default(true)
        .interact()?;
    let idle: u64 = Input::with_theme(&theme)
        .with_prompt("空闲间隔秒数")
        .default(1800)
        .interact_text()?;
    let mut value = read_config_table(paths)?;
    let archive_tbl = value
        .as_table_mut()
        .unwrap()
        .entry("archive".to_string())
        .or_insert(toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap();
    archive_tbl.insert("auto".to_string(), toml::Value::Boolean(auto));
    archive_tbl.insert("idle_secs".to_string(), toml::Value::Integer(idle as i64));
    write_config_table(paths, &value)?;
    Ok(())
}

fn trigger_archive(paths: &GarsPaths) -> Result<()> {
    let store = gars_store::Store::new(paths.home.join("gars.db"));
    store.init()?;
    let cfg = gars_archive::ArchiveConfig::default();
    let stats = gars_archive::run_idle_pass(paths, &store, &cfg)?;
    println!("归档完成：{} 个会话被处理。", stats.len());
    for s in stats {
        if s.skipped {
            println!(
                "  - skip {} ({})",
                s.source.display(),
                s.reason.unwrap_or_default()
            );
        } else {
            println!("  - {} → {}", s.source.display(), s.destination.display());
        }
    }
    Ok(())
}

fn read_config_table(paths: &GarsPaths) -> Result<toml::Value> {
    let content = fs::read_to_string(&paths.config)?;
    Ok(toml::from_str(&content)?)
}

fn write_config_table(paths: &GarsPaths, value: &toml::Value) -> Result<()> {
    let content = toml::to_string_pretty(value)?;
    fs::write(&paths.config, content)?;
    Ok(())
}

fn install_service(paths: &GarsPaths, yes: bool) -> Result<()> {
    let _ = ensure_gars_binary()?;
    if !yes {
        // Run the configure wizard first — fresh install must walk through
        // it; existing configs prompt before overwriting. `--yes` skips
        // both wizard and the confirm below for automation/install.sh.
        let needs_initial = !paths.config.exists();
        let rerun = if needs_initial {
            true
        } else {
            Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(
                    "已存在 ~/.gars/config.toml；先重新跑一次配置向导（LLM / 端口 / token）？\
                     选 No 直接用现有配置安装服务。",
                )
                .default(false)
                .interact()?
        };
        if rerun {
            configure(paths)?;
        }
        if !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("继续安装后台服务？")
            .default(true)
            .interact()?
        {
            return Ok(());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let launch_agents = dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir not found"))?
            .join("Library/LaunchAgents");
        fs::create_dir_all(&launch_agents)?;
        // v0.10 migration: clean up the legacy `cc.uoox.gars` plist (and
        // unload it from launchd) before writing the new one.
        let legacy_plist = launch_agents.join(format!("{LEGACY_LAUNCH_LABEL}.plist"));
        if legacy_plist.exists() {
            println!("→ 清理旧 LaunchAgent ({LEGACY_LAUNCH_LABEL})");
            if let Ok(uid) = command_output("id", ["-u"]) {
                let _ = run_status(
                    "launchctl",
                    [
                        "bootout",
                        &format!("gui/{}", uid.trim()),
                        LEGACY_LAUNCH_LABEL,
                    ],
                );
            }
            let _ = fs::remove_file(&legacy_plist);
        }
        let plist = macos_plist(paths)?;
        let plist_path = launch_agents.join(format!("{LAUNCH_LABEL}.plist"));
        println!("→ 写入 LaunchAgent: {}", plist_path.display());
        fs::write(&plist_path, plist)?;
        println!("✓ LaunchAgent 已安装 ({LAUNCH_LABEL})");
    }
    #[cfg(target_os = "linux")]
    {
        let dir = dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir not found"))?
            .join(".config/systemd/user");
        fs::create_dir_all(&dir)?;
        let unit_path = dir.join("gars.service");
        println!("→ 写入 systemd unit: {}", unit_path.display());
        fs::write(&unit_path, linux_unit(paths)?)?;
        println!("✓ systemd unit 已安装");
    }
    println!("✓ 服务文件已安装。可在菜单选「启动服务」启动。");
    Ok(())
}

fn uninstall_service(paths: &GarsPaths, yes: bool) -> Result<()> {
    if !yes
        && !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("卸载 gars 服务文件？不会删除 ~/.gars 数据。")
            .default(false)
            .interact()?
    {
        return Ok(());
    }
    println!("→ 停止运行中的服务");
    let _ = stop_service(paths);
    #[cfg(target_os = "macos")]
    {
        let launch_agents = dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir not found"))?
            .join("Library/LaunchAgents");
        for label in [LAUNCH_LABEL, LEGACY_LAUNCH_LABEL] {
            let path = launch_agents.join(format!("{label}.plist"));
            if path.exists() {
                println!("→ 删除 {}", path.display());
                let _ = fs::remove_file(&path);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let unit = dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir not found"))?
            .join(".config/systemd/user/gars.service");
        if unit.exists() {
            println!("→ 删除 {}", unit.display());
            let _ = fs::remove_file(unit);
        }
    }
    println!("✓ 服务文件已卸载。~/.gars 数据未动。");
    Ok(())
}

fn start_service(_paths: &GarsPaths) -> Result<()> {
    let bin = ensure_gars_binary()?;
    println!("→ 使用二进制: {}", bin.display());
    #[cfg(target_os = "macos")]
    {
        let uid = command_output("id", ["-u"])?;
        let plist = dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir not found"))?
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCH_LABEL}.plist"));
        // Bootout legacy label if loaded — avoids port conflicts when
        // upgrading from a pre-v0.10 install.
        let _ = run_status(
            "launchctl",
            [
                "bootout",
                &format!("gui/{}", uid.trim()),
                LEGACY_LAUNCH_LABEL,
            ],
        );
        println!("→ launchctl bootstrap {}", plist.display());
        let _ = run_status(
            "launchctl",
            [
                "bootstrap",
                &format!("gui/{}", uid.trim()),
                plist.to_str().unwrap_or(""),
            ],
        );
        println!("→ launchctl kickstart {LAUNCH_LABEL}");
        run_status(
            "launchctl",
            [
                "kickstart",
                "-k",
                &format!("gui/{}/{}", uid.trim(), LAUNCH_LABEL),
            ],
        )?;
    }
    #[cfg(target_os = "linux")]
    {
        println!("→ systemctl --user enable --now gars.service");
        if run_status("systemctl", ["--user", "enable", "--now", "gars.service"]).is_err() {
            println!("→ systemctl 不可用，退到 fallback spawn");
            spawn_fallback(_paths)?;
        }
    }
    println!("✓ 启动命令已执行。可用「查看状态」检查 /health。");
    Ok(())
}

fn stop_service(paths: &GarsPaths) -> Result<()> {
    #[cfg(target_os = "macos")]
    let _ = paths;
    #[cfg(target_os = "macos")]
    {
        let uid = command_output("id", ["-u"])?;
        // Try bootout both new + legacy labels; whichever isn't loaded
        // returns non-zero, which we ignore.
        for label in [LAUNCH_LABEL, LEGACY_LAUNCH_LABEL] {
            let _ = run_status(
                "launchctl",
                ["bootout", &format!("gui/{}", uid.trim()), label],
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = run_status("systemctl", ["--user", "disable", "--now", "gars.service"]);
        stop_fallback(paths)?;
    }
    println!("✓ 停止命令已执行。");
    Ok(())
}

async fn status(paths: &GarsPaths) -> Result<()> {
    let cfg = fs::read_to_string(&paths.config)?;
    let value: toml::Value = toml::from_str(&cfg)?;
    let token = value
        .get("server")
        .and_then(|s| s.get("admin_token"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let port = value
        .get("server")
        .and_then(|s| s.get("port"))
        .and_then(|v| v.as_integer())
        .unwrap_or(9221);
    let url = format!("http://127.0.0.1:{port}/v1/status");
    println!("→ GET {url}");
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await;
    match response {
        Ok(resp) if resp.status().is_success() => {
            let status_code = resp.status();
            let body: Value = resp.json().await?;
            println!("✓ 服务正常 ({status_code})");
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Ok(resp) => println!("✗ 服务响应异常: {}", resp.status()),
        Err(err) => println!("✗ 连不上 127.0.0.1:{port}: {err}"),
    }
    Ok(())
}

fn show_logs(paths: &GarsPaths) -> Result<()> {
    let stdout = paths.logs.join("gars.out.log");
    let stderr = paths.logs.join("gars.err.log");
    for path in [stdout, stderr] {
        println!();
        println!("==> {} (最后 80 行)", path.display());
        match fs::read_to_string(&path) {
            Ok(content) if content.trim().is_empty() => println!("(空)"),
            Ok(content) => println!("{}", tail(&content, 80)),
            Err(err) => println!("(读取失败: {err})"),
        }
    }
    Ok(())
}

fn offer_legacy_migration(paths: &GarsPaths) -> Result<()> {
    let marker = paths.home.join(".legacy_ga_migration_checked");
    if marker.exists() {
        return Ok(());
    }
    migrate_legacy(paths, false)?;
    fs::write(marker, "checked\n")?;
    Ok(())
}

fn migrate_legacy(paths: &GarsPaths, yes: bool) -> Result<()> {
    let Some(legacy) = legacy_home() else {
        return Ok(());
    };
    if !legacy.exists() {
        println!("未找到 {}", legacy.display());
        return Ok(());
    }
    if !yes
        && !Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!(
                "检测到旧目录 {}，是否把缺失文件复制到 {}？已有文件不会覆盖。",
                legacy.display(),
                paths.home.display()
            ))
            .default(false)
            .interact()?
    {
        return Ok(());
    }
    let (copied, skipped) = copy_dir_missing(&legacy, &paths.home)?;
    println!("迁移完成：复制 {copied} 个文件，跳过 {skipped} 个已存在文件。");
    Ok(())
}

/// Resolve where the `gars` binary lives.
///
/// Strategy (in order):
/// 1. `GARS_BIN_DIR` env var, if set, joined with `gars`.
/// 2. Sibling of the currently-running garstool exe — covers both the
///    install.sh deploy (both at `/usr/local/bin/`) and the dev path
///    (`target/debug/`).
/// 3. Fall back to `/usr/local/bin/gars`.
fn resolve_gars_binary() -> PathBuf {
    if let Ok(dir) = std::env::var("GARS_BIN_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("gars");
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let sibling = parent.join("gars");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("/usr/local/bin/gars")
}

fn ensure_gars_binary() -> Result<PathBuf> {
    let p = resolve_gars_binary();
    if !p.exists() {
        return Err(anyhow!(
            "gars binary not found at {} (set GARS_BIN_DIR or reinstall)",
            p.display()
        ));
    }
    Ok(p)
}

#[cfg(target_os = "macos")]
fn macos_plist(paths: &GarsPaths) -> Result<String> {
    let gars = ensure_gars_binary()?;
    let out = paths.logs.join("gars.out.log");
    let err = paths.logs.join("gars.err.log");
    fs::create_dir_all(&paths.logs)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.github.uoox.gars</string>
  <key>ProgramArguments</key><array><string>{}</string><string>--c</string><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict></plist>
"#,
        gars.display(),
        paths.config.display(),
        out.display(),
        err.display()
    ))
}

#[cfg(target_os = "linux")]
fn linux_unit(paths: &GarsPaths) -> Result<String> {
    let gars = ensure_gars_binary()?;
    fs::create_dir_all(&paths.logs)?;
    Ok(format!(
        r#"[Unit]
Description=gars local agent service
After=network.target

[Service]
ExecStart={} --c {}
Restart=always
RestartSec=3
StandardOutput=append:{}
StandardError=append:{}

[Install]
WantedBy=default.target
"#,
        gars.display(),
        paths.config.display(),
        paths.logs.join("gars.out.log").display(),
        paths.logs.join("gars.err.log").display()
    ))
}

#[cfg(not(target_os = "macos"))]
fn spawn_fallback(paths: &GarsPaths) -> Result<()> {
    let bin = ensure_gars_binary()?;
    let child = Command::new(&bin)
        .arg("--c")
        .arg(&paths.config)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    fs::write(paths.home.join("gars.pid"), child.id().to_string())?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn stop_fallback(paths: &GarsPaths) -> Result<()> {
    let pid_path = paths.home.join("gars.pid");
    if let Ok(pid) = fs::read_to_string(&pid_path) {
        let pid = pid.trim();
        let _ = Command::new("kill").arg(pid).status();
        let _ = fs::remove_file(pid_path);
    }
    Ok(())
}

fn run_status<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{program} exited with {status}"))
    }
}

#[cfg(target_os = "macos")]
fn command_output<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(program).args(args).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn tail(content: &str, count: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

fn copy_dir_missing(from: &Path, to: &Path) -> Result<(usize, usize)> {
    fs::create_dir_all(to)?;
    let mut copied = 0;
    let mut skipped = 0;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            let (child_copied, child_skipped) = copy_dir_missing(&src, &dst)?;
            copied += child_copied;
            skipped += child_skipped;
        } else if dst.exists() {
            skipped += 1;
        } else {
            fs::copy(src, dst)?;
            copied += 1;
        }
    }
    Ok((copied, skipped))
}

#[allow(dead_code)]
fn write_default_if_missing(paths: &GarsPaths) -> Result<()> {
    if !paths.config.exists() {
        fs::write(&paths.config, default_config())?;
    }
    Ok(())
}
