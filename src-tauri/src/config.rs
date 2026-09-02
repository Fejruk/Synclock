use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::RwLock;

static CONFIG: OnceLock<RwLock<AppConfig>> = OnceLock::new();

fn config_lock() -> &'static RwLock<AppConfig> {
    CONFIG.get_or_init(|| RwLock::new(AppConfig::default()))
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("synclock")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_provider")]
    pub provider: String, // "early" or "toggl"

    // Early
    #[serde(default)]
    pub early_api_key: String,
    #[serde(default)]
    pub early_api_secret: String,

    // Toggl
    #[serde(default)]
    pub toggl_api_token: String,

    // Target tracker selection
    #[serde(default = "default_target")]
    pub target: String, // "jira" or "youtrack"

    // Jira
    #[serde(default)]
    pub jira_base_url: String,
    #[serde(default)]
    pub jira_email: String,
    #[serde(default)]
    pub jira_api_token: String,

    // YouTrack
    #[serde(default)]
    pub youtrack_base_url: String,
    #[serde(default)]
    pub youtrack_token: String,

    // Fallback issue key used when a time entry has no detected issue key.
    // Empty = entries without a key are skipped (the original behavior).
    #[serde(default)]
    pub default_issue_key: String,

    // Mapping: Early activity id → YouTrack work item type id (empty value = no type)
    #[serde(default)]
    pub activity_type_map: HashMap<String, String>,

    // Daily auto-sync
    #[serde(default)]
    pub auto_sync_enabled: bool,
    #[serde(default = "default_auto_sync_time")]
    pub auto_sync_time: String, // "HH:MM" format

    // Tray icon style: "color" or "mono"
    #[serde(default = "default_tray_icon")]
    pub tray_icon: String,
}

fn default_provider() -> String { "early".into() }
fn default_target() -> String { "jira".into() }
fn default_auto_sync_time() -> String { "19:00".into() }
fn default_tray_icon() -> String { "color".into() }

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            early_api_key: String::new(),
            early_api_secret: String::new(),
            toggl_api_token: String::new(),
            target: default_target(),
            jira_base_url: String::new(),
            jira_email: String::new(),
            jira_api_token: String::new(),
            youtrack_base_url: String::new(),
            youtrack_token: String::new(),
            default_issue_key: String::new(),
            activity_type_map: HashMap::new(),
            auto_sync_enabled: false,
            auto_sync_time: default_auto_sync_time(),
            tray_icon: default_tray_icon(),
        }
    }
}

impl AppConfig {
    pub fn is_configured(&self) -> bool {
        let has_provider = match self.provider.as_str() {
            "early" => !self.early_api_key.is_empty() && !self.early_api_secret.is_empty(),
            "toggl" => !self.toggl_api_token.is_empty(),
            _ => false,
        };
        let has_target = match self.target.as_str() {
            "youtrack" => !self.youtrack_base_url.is_empty() && !self.youtrack_token.is_empty(),
            _ => !self.jira_base_url.is_empty()
                && !self.jira_email.is_empty()
                && !self.jira_api_token.is_empty(),
        };
        has_provider && has_target
    }
}

/// Load config from file. A missing or unreadable file yields defaults.
pub fn load_config() {
    let cfg = fs::read_to_string(config_path())
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default();

    // Also set env vars for backward compat with early.rs / jira.rs
    apply_to_env(&cfg);

    let lock = config_lock();
    *lock.blocking_write() = cfg;
}

fn apply_to_env(cfg: &AppConfig) {
    std::env::set_var("EARLY_API_KEY", &cfg.early_api_key);
    std::env::set_var("EARLY_API_SECRET", &cfg.early_api_secret);
    std::env::set_var("TOGGL_API_TOKEN", &cfg.toggl_api_token);
    std::env::set_var("JIRA_BASE_URL", &cfg.jira_base_url);
    std::env::set_var("JIRA_EMAIL", &cfg.jira_email);
    std::env::set_var("JIRA_API_TOKEN", &cfg.jira_api_token);
}

fn save_config_to_file(cfg: &AppConfig) -> Result<(), String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn get_config() -> AppConfig {
    config_lock().read().await.clone()
}

pub async fn save_config(cfg: AppConfig) -> Result<(), String> {
    apply_to_env(&cfg);
    save_config_to_file(&cfg)?;

    // Clear cached tokens since credentials may have changed
    crate::early::clear_token_cache().await;
    crate::jira::clear_cloud_id_cache().await;
    crate::youtrack::clear_alias_cache().await;

    *config_lock().write().await = cfg;
    Ok(())
}
