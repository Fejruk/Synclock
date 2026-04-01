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

    // Parse any timestamp format and output Jira format: yyyy-MM-dd'T'HH:mm:ss.SSS+0000
    let started_formatted = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(started) {
        // Toggl: "2026-04-01T07:00:00+00:00"
        dt.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%S%.3f+0000").to_string()
    } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(started, "%Y-%m-%dT%H:%M:%S%.3f") {
        // Early: "2026-04-01T07:00:00.000" (UTC without Z)
        format!("{}+0000", dt.format("%Y-%m-%dT%H:%M:%S%.3f"))
    } else {
        // Fallback
        format!("{}+0000", started)
    };

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

pub fn is_already_synced(worklogs: &[Worklog], started_at: &str, duration_min: i64) -> bool {
    // Parse any format: RFC3339 (Toggl) or naive (Early UTC)
    let early_start = chrono::DateTime::parse_from_rfc3339(started_at)
        .map(|dt| dt.timestamp_millis())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(started_at, "%Y-%m-%dT%H:%M:%S%.3f")
                .map(|dt| dt.and_utc().timestamp_millis())
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wl(started: &str, seconds: i64) -> Worklog {
        Worklog { id: "1".into(), started: started.into(), time_spent_seconds: seconds }
    }

    // ── is_already_synced ──

    #[test]
    fn exact_match() {
        let worklogs = vec![wl("2026-04-01T07:00:00.000+0000", 1800)];
        assert!(is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn no_worklogs_not_synced() {
        assert!(!is_already_synced(&[], "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn different_time_not_synced() {
        let worklogs = vec![wl("2026-04-01T10:00:00.000+0000", 1800)];
        assert!(!is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn different_duration_not_synced() {
        let worklogs = vec![wl("2026-04-01T07:00:00.000+0000", 7200)]; // 2h
        assert!(!is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30)); // 30m
    }

    #[test]
    fn within_2min_tolerance_matches() {
        // Jira started 1 minute later
        let worklogs = vec![wl("2026-04-01T07:01:00.000+0000", 1800)];
        assert!(is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn beyond_2min_tolerance_no_match() {
        // Jira started 3 minutes later
        let worklogs = vec![wl("2026-04-01T07:03:00.000+0000", 1800)];
        assert!(!is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn duration_within_60s_tolerance() {
        // 1830 seconds = 30m 30s, entry is 30m = 1800s — diff is 30s < 60s
        let worklogs = vec![wl("2026-04-01T07:00:00.000+0000", 1830)];
        assert!(is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn duration_beyond_60s_tolerance() {
        // 1900 seconds vs 1800 — diff is 100s > 60s
        let worklogs = vec![wl("2026-04-01T07:00:00.000+0000", 1900)];
        assert!(!is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn matches_with_timezone_offset() {
        // Jira returns +0200 (CEST), Early is UTC
        // 09:00+0200 = 07:00 UTC — should match 07:00 UTC
        let worklogs = vec![wl("2026-04-01T09:00:00.000+0200", 1800)];
        assert!(is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }

    #[test]
    fn multiple_worklogs_finds_match() {
        let worklogs = vec![
            wl("2026-04-01T10:00:00.000+0000", 3600),
            wl("2026-04-01T07:00:00.000+0000", 1800), // this one matches
            wl("2026-04-01T14:00:00.000+0000", 900),
        ];
        assert!(is_already_synced(&worklogs, "2026-04-01T07:00:00.000", 30));
    }
}
