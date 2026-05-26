use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;

static ALIAS_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn alias_cache() -> &'static Mutex<HashMap<String, String>> {
    ALIAS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn clear_alias_cache() {
    alias_cache().lock().await.clear();
}

async fn config_async() -> Result<(String, String), String> {
    let cfg = crate::config::get_config().await;
    if cfg.youtrack_base_url.is_empty() {
        return Err("YouTrack base URL not set".into());
    }
    if cfg.youtrack_token.is_empty() {
        return Err("YouTrack token not set".into());
    }
    Ok((
        cfg.youtrack_base_url.trim_end_matches('/').to_string(),
        cfg.youtrack_token,
    ))
}

fn auth_headers(token: &str) -> Result<HeaderMap, String> {
    let mut h = HeaderMap::new();
    let value = HeaderValue::from_str(&format!("Bearer {}", token))
        .map_err(|e| format!("Invalid YouTrack token: {}", e))?;
    h.insert(AUTHORIZATION, value);
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(h)
}

#[allow(dead_code)] // fields populated via serde, only `text` is currently consumed
#[derive(Debug, Clone, Deserialize)]
pub struct WorkItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub duration: WorkItemDuration,
    #[serde(default)]
    pub text: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkItemDuration {
    #[serde(default)]
    pub minutes: i64,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct WorkItemType {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

pub async fn get_work_item_types() -> Result<Vec<WorkItemType>, String> {
    let (base, token) = config_async().await?;
    let client = reqwest::Client::new();

    // Try the admin endpoint first (global list across all projects).
    let url = format!(
        "{}/api/admin/timeTrackingSettings/workItemTypes?fields=id,name",
        base
    );
    let resp = client
        .get(&url)
        .headers(auth_headers(&token)?)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        let mut types: Vec<WorkItemType> = resp.json().await.map_err(|e| e.to_string())?;
        types.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        return Ok(types);
    }

    // Fallback: aggregate types from all projects (needs less privilege).
    #[derive(Deserialize)]
    struct ProjectTts {
        #[serde(default, rename = "workItemTypes")]
        work_item_types: Vec<WorkItemType>,
    }
    #[derive(Deserialize)]
    struct ProjectWithTts {
        #[serde(default, rename = "timeTrackingSettings")]
        tts: Option<ProjectTts>,
    }

    let fb_url = format!(
        "{}/api/admin/projects?fields=timeTrackingSettings(workItemTypes(id,name))",
        base
    );
    let fb = client
        .get(&fb_url)
        .headers(auth_headers(&token)?)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !fb.status().is_success() {
        return Err(format!("Failed to load YouTrack work item types ({})", fb.status()));
    }
    let projects: Vec<ProjectWithTts> = fb.json().await.map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<WorkItemType> = Vec::new();
    for p in projects {
        if let Some(tts) = p.tts {
            for t in tts.work_item_types {
                if seen.insert(t.id.clone()) {
                    out.push(t);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub async fn test_connection() -> Result<(), String> {
    let (base, token) = config_async().await?;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/users/me?fields=login", base))
        .headers(auth_headers(&token)?)
        .send()
        .await
        .map_err(|e| format!("YouTrack connection failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("YouTrack auth failed ({})", resp.status()));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct IssueIdResponse {
    #[serde(rename = "idReadable", default)]
    id_readable: String,
}

/// Resolve a possibly-foreign issue key (e.g. legacy Jira `IS-6562`) to the
/// actual YouTrack readable id. If the key already exists in YouTrack, returns it
/// unchanged. Otherwise searches for an issue whose body contains
/// `Migrated from JIRA: <key>` and returns that issue's idReadable. Caches results
/// for the rest of the session.
pub async fn resolve_issue_id(issue_key: &str) -> Result<String, String> {
    {
        let cache = alias_cache().lock().await;
        if let Some(resolved) = cache.get(issue_key) {
            return Ok(resolved.clone());
        }
    }

    let (base, token) = config_async().await?;
    let client = reqwest::Client::new();

    // 1) Try the key directly.
    let direct = client
        .get(format!("{}/api/issues/{}?fields=idReadable", base, issue_key))
        .headers(auth_headers(&token)?)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if direct.status().is_success() {
        let info: IssueIdResponse = direct.json().await.map_err(|e| e.to_string())?;
        let resolved = if info.id_readable.is_empty() {
            issue_key.to_string()
        } else {
            info.id_readable
        };
        alias_cache().lock().await.insert(issue_key.to_string(), resolved.clone());
        return Ok(resolved);
    }

    // 2) Fall back to text search for the migration marker.
    let query = format!("Migrated from JIRA: {}", issue_key);
    let search_url = format!(
        "{}/api/issues?fields=idReadable&$top=5&query={}",
        base,
        urlencoding_encode(&query)
    );
    let resp = client
        .get(&search_url)
        .headers(auth_headers(&token)?)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!(
            "YouTrack issue '{}' not found and search failed ({})",
            issue_key,
            resp.status()
        ));
    }

    let candidates: Vec<IssueIdResponse> = resp.json().await.map_err(|e| e.to_string())?;
    let resolved = candidates
        .into_iter()
        .map(|c| c.id_readable)
        .find(|s| !s.is_empty())
        .ok_or_else(|| format!("YouTrack issue '{}' not found (no migration alias either)", issue_key))?;

    alias_cache()
        .lock()
        .await
        .insert(issue_key.to_string(), resolved.clone());
    Ok(resolved)
}

/// Minimal URL component encoder for YouTrack query strings (spaces, ':', etc.).
fn urlencoding_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

pub async fn get_work_items(issue_key: &str) -> Result<Vec<WorkItem>, String> {
    let resolved = resolve_issue_id(issue_key).await?;
    let (base, token) = config_async().await?;
    let client = reqwest::Client::new();

    // YouTrack caps the response at a default page size (42) when `$top` is
    // omitted. High-volume issues (e.g. shared "management" buckets) accumulate
    // far more work items than that, so the most recent ones — including our
    // synclock dedup markers — fall outside the first page and the entry looks
    // unsynced even though it was already logged. Page explicitly to get them all.
    const PAGE: usize = 200;
    let mut all: Vec<WorkItem> = Vec::new();
    let mut skip = 0usize;
    loop {
        let url = format!(
            "{}/api/issues/{}/timeTracking/workItems?fields=id,date,duration(minutes),text&$top={}&$skip={}",
            base, resolved, PAGE, skip
        );
        let resp = client
            .get(&url)
            .headers(auth_headers(&token)?)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Ok(all);
        }

        let page = resp.json::<Vec<WorkItem>>().await.map_err(|e| e.to_string())?;
        let got = page.len();
        all.extend(page);
        if got < PAGE {
            break;
        }
        skip += PAGE;
    }
    Ok(all)
}

/// Convert a time entry's start timestamp into the ms-since-epoch of UTC midnight
/// of the *local* calendar day that contains it. Mirrors the "today" logic used by
/// `start_auto_sync` (chrono::Local).
fn started_to_local_day_ms(started_at: &str) -> i64 {
    let utc_dt: DateTime<Utc> = if let Ok(dt) = DateTime::parse_from_rfc3339(started_at) {
        dt.with_timezone(&Utc)
    } else if let Ok(naive) = NaiveDateTime::parse_from_str(started_at, "%Y-%m-%dT%H:%M:%S%.3f") {
        naive.and_utc()
    } else if let Ok(naive) = NaiveDateTime::parse_from_str(started_at, "%Y-%m-%dT%H:%M:%S") {
        naive.and_utc()
    } else {
        Utc::now()
    };
    let local_date = utc_dt.with_timezone(&Local).date_naive();
    let local_midnight = local_date.and_hms_opt(0, 0, 0).unwrap();
    Utc.from_utc_datetime(&local_midnight).timestamp_millis()
}

pub async fn add_work_item(
    issue_key: &str,
    minutes: i64,
    started_at: &str,
    comment: &str,
    dedup_marker: &str,
    type_id: Option<&str>,
) -> Result<String, String> {
    let resolved = resolve_issue_id(issue_key).await?;
    let (base, token) = config_async().await?;
    let date_ms = started_to_local_day_ms(started_at);

    let text = if comment.is_empty() {
        format!("[synclock:{}]", dedup_marker)
    } else {
        format!("{}\n\n[synclock:{}]", comment, dedup_marker)
    };

    let mut body = serde_json::json!({
        "date": date_ms,
        "duration": { "minutes": minutes },
        "text": text,
    });
    if let Some(tid) = type_id {
        if !tid.is_empty() {
            body["type"] = serde_json::json!({ "id": tid });
        }
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/api/issues/{}/timeTracking/workItems?fields=id",
            base, resolved
        ))
        .headers(auth_headers(&token)?)
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

pub fn is_already_synced(items: &[WorkItem], dedup_marker: &str) -> bool {
    let needle = format!("[synclock:{}]", dedup_marker);
    items.iter().any(|w| w.text.contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str) -> WorkItem {
        WorkItem {
            id: "1".into(),
            date: 0,
            duration: WorkItemDuration { minutes: 30 },
            text: text.into(),
        }
    }

    #[test]
    fn finds_marker() {
        let items = vec![item("Standup\n\n[synclock:toggl-123]")];
        assert!(is_already_synced(&items, "toggl-123"));
    }

    #[test]
    fn no_marker() {
        let items = vec![item("Just a regular work item")];
        assert!(!is_already_synced(&items, "toggl-123"));
    }

    #[test]
    fn different_marker() {
        let items = vec![item("[synclock:early-abc]")];
        assert!(!is_already_synced(&items, "toggl-123"));
    }

    #[test]
    fn marker_in_middle() {
        let items = vec![item("Pre [synclock:toggl-123] post")];
        assert!(is_already_synced(&items, "toggl-123"));
    }

    #[test]
    fn empty_items() {
        assert!(!is_already_synced(&[], "toggl-123"));
    }

    #[test]
    fn provider_prefix_disambiguates() {
        // Same numeric id but different provider — must not match.
        let items = vec![item("[synclock:early-123]")];
        assert!(!is_already_synced(&items, "toggl-123"));
    }
}
