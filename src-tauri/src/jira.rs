use base64::Engine;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::sync::OnceLock;
use tokio::sync::Mutex;

static CLOUD_ID: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn cloud_id_mutex() -> &'static Mutex<Option<String>> {
    CLOUD_ID.get_or_init(|| Mutex::new(None))
}

pub async fn clear_cloud_id_cache() {
    *cloud_id_mutex().lock().await = None;
}

async fn get_cloud_id() -> Result<String, String> {
    let mut cached = cloud_id_mutex().lock().await;
    if let Some(ref id) = *cached {
        return Ok(id.clone());
    }

    let base_url = std::env::var("JIRA_BASE_URL")
        .map_err(|_| "JIRA_BASE_URL not set".to_string())?;
    let url = format!("{}/_edge/tenant_info", base_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("Failed to resolve Jira Cloud ID from {}", base_url));
    }

    #[derive(Deserialize)]
    struct TenantInfo { #[serde(rename = "cloudId")] cloud_id: String }

    let info: TenantInfo = resp.json().await.map_err(|e| e.to_string())?;
    *cached = Some(info.cloud_id.clone());
    Ok(info.cloud_id)
}

async fn get_api_base() -> Result<String, String> {
    let cloud_id = get_cloud_id().await?;
    Ok(format!("https://api.atlassian.com/ex/jira/{}", cloud_id))
}

fn get_auth_header() -> Result<String, String> {
    let email = std::env::var("JIRA_EMAIL")
        .map_err(|_| "JIRA_EMAIL not set".to_string())?;
    let token = std::env::var("JIRA_API_TOKEN")
        .map_err(|_| "JIRA_API_TOKEN not set".to_string())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", email, token));
    Ok(format!("Basic {}", encoded))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Worklog {
    pub id: String,
    pub started: String,
    #[serde(rename = "timeSpentSeconds")]
    pub time_spent_seconds: i64,
}

#[derive(Debug, Deserialize)]
struct WorklogsResponse {
    #[serde(default)]
    worklogs: Vec<Worklog>,
}

pub async fn get_worklogs(issue_key: &str) -> Result<Vec<Worklog>, String> {
    let base = get_api_base().await?;
    let auth = get_auth_header()?;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/rest/api/3/issue/{}/worklog", base, issue_key))
        .header(AUTHORIZATION, &auth)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Ok(vec![]);
    }

    let data: WorklogsResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(data.worklogs)
}

pub async fn add_worklog(
    issue_key: &str,
    time_spent_minutes: i64,
    started: &str,
    comment: &str,
) -> Result<String, String> {
    let base = get_api_base().await?;
    let auth = get_auth_header()?;

    let hours = time_spent_minutes / 60;
    let mins = time_spent_minutes % 60;
    let time_spent = match (hours, mins) {
        (0, m) => format!("{}m", m),
        (h, 0) => format!("{}h", h),
        (h, m) => format!("{}h {}m", h, m),
    };

    // Early timestamps are UTC without Z — parse and format for Jira
    let started_formatted = format!("{}+0000", started);

    let mut body = serde_json::json!({
        "timeSpent": time_spent,
        "started": started_formatted,
    });

    if !comment.is_empty() {
        body["comment"] = serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "text", "text": comment }]
            }]
        });
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/rest/api/3/issue/{}/worklog", base, issue_key))
        .header(AUTHORIZATION, &auth)
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, text));
    }

    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(data["id"].as_str().unwrap_or("").to_string())
}

pub async fn test_connection() -> Result<(), String> {
    let base = get_api_base().await?;
    let auth = get_auth_header()?;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/rest/api/3/myself", base))
        .header(AUTHORIZATION, &auth)
        .send()
        .await
        .map_err(|e| format!("Jira connection failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Jira auth failed ({})", resp.status()));
    }

    Ok(())
}

pub fn is_already_synced(worklogs: &[Worklog], early_started_at: &str, duration_min: i64) -> bool {
    // Early timestamp is UTC without Z
    let early_start = chrono::NaiveDateTime::parse_from_str(early_started_at, "%Y-%m-%dT%H:%M:%S%.3f")
        .map(|dt| dt.and_utc().timestamp_millis())
        .unwrap_or(0);
    let early_duration_sec = duration_min * 60;

    worklogs.iter().any(|wl| {
        let jira_start = chrono::DateTime::parse_from_str(&wl.started, "%Y-%m-%dT%H:%M:%S%.3f%z")
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(1); // different default so no false match
        let time_diff = (jira_start - early_start).abs();
        let dur_diff = (wl.time_spent_seconds - early_duration_sec).abs();
        time_diff < 120_000 && dur_diff < 60
    })
}
