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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(desc: Option<&str>, tags: Option<Vec<&str>>) -> TogglTimeEntry {
        TogglTimeEntry {
            id: 1,
            description: desc.map(|s| s.into()),
            start: "2026-04-01T07:00:00Z".into(),
            stop: Some("2026-04-01T07:30:00Z".into()),
            duration: 1800,
            project_id: None,
            tags: tags.map(|t| t.into_iter().map(|s| s.into()).collect()),
        }
    }

    #[test]
    fn extract_from_description() {
        let e = entry(Some("TICK-301 Standup"), None);
        let keys = extract_jira_keys(&e);
        assert_eq!(keys, vec!["TICK-301"]);
    }

    #[test]
    fn extract_from_tags() {
        let e = entry(Some("Standup"), Some(vec!["TICK-301"]));
        let keys = extract_jira_keys(&e);
        assert_eq!(keys, vec!["TICK-301"]);
    }

    #[test]
    fn extract_multiple_from_description() {
        let e = entry(Some("BACK-7 and FE-19 work"), None);
        let mut keys = extract_jira_keys(&e);
        keys.sort();
        assert_eq!(keys, vec!["BACK-7", "FE-19"]);
    }

    #[test]
    fn no_keys_found() {
        let e = entry(Some("just a meeting"), None);
        assert!(extract_jira_keys(&e).is_empty());
    }

    #[test]
    fn no_description() {
        let e = entry(None, None);
        assert!(extract_jira_keys(&e).is_empty());
    }

    #[test]
    fn deduplicates_across_desc_and_tags() {
        let e = entry(Some("BACK-7 work"), Some(vec!["BACK-7"]));
        let keys = extract_jira_keys(&e);
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn ignores_lowercase() {
        let e = entry(Some("is-123 not valid"), None);
        assert!(extract_jira_keys(&e).is_empty());
    }
}
