use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

const BASE_URL: &str = "https://api.track.toggl.com/api/v9";

fn get_auth_header() -> Result<String, String> {
    let token = std::env::var("TOGGL_API_TOKEN")
        .map_err(|_| "TOGGL_API_TOKEN not set".to_string())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{}:api_token", token));
    Ok(format!("Basic {}", encoded))
}

#[derive(Debug, Clone, Deserialize)]
pub struct TogglTimeEntry {
    pub id: i64,
    pub description: Option<String>,
    pub start: String,
    pub stop: Option<String>,
    pub duration: i64, // seconds, negative if running
    pub project_id: Option<i64>,
    pub tags: Option<Vec<String>>,
}

/// Fetch time entries for a date range.
/// Toggl API: GET /me/time_entries?start_date=...&end_date=...
pub async fn get_time_entries(from: &str, to: &str) -> Result<Vec<TogglTimeEntry>, String> {
    let auth = get_auth_header()?;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/me/time_entries", BASE_URL))
        .header(AUTHORIZATION, &auth)
        .header(CONTENT_TYPE, "application/json")
        .query(&[
            ("start_date", format!("{}T00:00:00Z", from)),
            ("end_date", format!("{}T23:59:59Z", to)),
        ])
        .send()
        .await
        .map_err(|e| format!("Toggl API error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Toggl API error ({}): {}", status, body));
    }

    let entries: Vec<TogglTimeEntry> = resp.json().await.map_err(|e| e.to_string())?;
    // Filter out running entries (negative duration)
    Ok(entries.into_iter().filter(|e| e.duration > 0).collect())
}

/// Extract Jira issue keys from entry description and tags
pub fn extract_jira_keys(entry: &TogglTimeEntry) -> Vec<String> {
    let re = regex_lite::Regex::new(r"\b([A-Z][A-Z0-9]+-\d+)\b").unwrap();
    let mut keys = std::collections::HashSet::new();

    if let Some(desc) = &entry.description {
        for m in re.find_iter(desc) {
            keys.insert(m.as_str().to_string());
        }
    }
    if let Some(tags) = &entry.tags {
        for tag in tags {
            for m in re.find_iter(tag) {
                keys.insert(m.as_str().to_string());
            }
        }
    }

    keys.into_iter().collect()
}

pub async fn test_connection() -> Result<(), String> {
    let auth = get_auth_header()?;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/me", BASE_URL))
        .header(AUTHORIZATION, &auth)
        .header(CONTENT_TYPE, "application/json")
        .send()
        .await
        .map_err(|e| format!("Toggl connection failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Toggl auth failed ({})", resp.status()));
    }

    Ok(())
}
