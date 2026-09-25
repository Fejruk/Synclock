use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;

use crate::reconcile::{compose_text, Change, Desired};

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
    #[serde(default, rename = "type")]
    pub item_type: Option<IdRef>,
    #[serde(default)]
    pub issue: Option<IssueRef>,
    #[serde(default)]
    pub author: Option<IdRef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkItemDuration {
    #[serde(default)]
    pub minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdRef {
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueRef {
    #[serde(rename = "idReadable", default)]
    pub id_readable: String,
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

/// Resolve a possibly-foreign issue key (e.g. a legacy Jira key like `PROJ-123`) to the
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

    // Only a definite "no such issue" may fall through to the alias search. A
    // transient failure (429, 5xx, …) must stay an error: a guessed alias would
    // make the sync move the entry's work items to the wrong issue.
    if direct.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(format!("YouTrack issue '{}' lookup failed ({})", issue_key, direct.status()));
    }

    // 2) Fall back to an exact-phrase search for the migration marker.
    let query = format!("\"Migrated from JIRA: {}\"", issue_key);
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
    let mut found: Vec<String> = candidates.into_iter().map(|c| c.id_readable).filter(|s| !s.is_empty()).collect();
    found.dedup();
    let resolved = match found.as_slice() {
        [one] => one.clone(),
        [] => return Err(format!("YouTrack issue '{}' not found (no migration alias either)", issue_key)),
        many => return Err(format!("YouTrack issue '{}' is ambiguous: {} all mention it as migrated", issue_key, many.join(", "))),
    };

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

/// All work items authored by the token's user dated within `from..=to`
/// (`YYYY-MM-DD`, inclusive), across every issue. Fails instead of returning a
/// partial list — the caller deletes items it doesn't see an entry for, so a
/// silently truncated list must never be mistaken for the full picture.
pub async fn get_my_work_items(from: &str, to: &str) -> Result<Vec<WorkItem>, String> {
    let (base, token) = config_async().await?;
    let client = reqwest::Client::new();

    // `author=me` filters server-side; the author is checked again below so a
    // server ignoring the parameter can never hand us colleagues' items to delete.
    let me: IdRef = send_checked(
        client.get(format!("{}/api/users/me?fields=id", base)).headers(auth_headers(&token)?),
    )
    .await?
    .json()
    .await
    .map_err(|e| e.to_string())?;
    if me.id.is_empty() {
        return Err("Could not determine the YouTrack user".into());
    }

    const PAGE: usize = 500;
    let mut all: Vec<WorkItem> = Vec::new();
    let mut skip = 0usize;
    loop {
        let url = format!(
            "{}/api/workItems?author=me&startDate={}&endDate={}&fields=id,date,duration(minutes),text,type(id),issue(idReadable),author(id)&$top={}&$skip={}",
            base, from, to, PAGE, skip
        );
        let resp = client
            .get(&url)
            .headers(auth_headers(&token)?)
            .send()
            .await
            .map_err(|e| format!("YouTrack work items request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Failed to load YouTrack work items ({}): {}", status, body));
        }

        let page = resp.json::<Vec<WorkItem>>().await.map_err(|e| e.to_string())?;
        let got = page.len();
        all.extend(page);
        if got < PAGE {
            break;
        }
        skip += PAGE;
    }
    Ok(all
        .into_iter()
        .filter(|w| w.author.as_ref().is_some_and(|a| a.id == me.id))
        .collect())
}

/// Convert a time entry's start timestamp into the ms-since-epoch of UTC midnight
/// of the *local* calendar day that contains it. Mirrors the "today" logic used by
/// `start_auto_sync` (chrono::Local).
pub fn started_to_local_day_ms(started_at: &str) -> i64 {
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

async fn send_checked(req: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, text));
    }
    Ok(resp)
}

/// Create a work item on `desired.issue` (already a resolved readable id).
pub async fn create_work_item(desired: &Desired, provider_marker: &str) -> Result<String, String> {
    let (base, token) = config_async().await?;

    let mut body = serde_json::json!({
        "date": desired.date_ms,
        "duration": { "minutes": desired.minutes },
        "text": compose_text(&desired.text, provider_marker),
    });
    if let Some(tid) = &desired.type_id {
        body["type"] = serde_json::json!({ "id": tid });
    }

    let resp = send_checked(
        reqwest::Client::new()
            .post(format!("{}/api/issues/{}/timeTracking/workItems?fields=id", base, desired.issue))
            .headers(auth_headers(&token)?)
            .json(&body),
    )
    .await?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(data["id"].as_str().unwrap_or("").to_string())
}

/// Update only the fields listed in `changes`.
pub async fn update_work_item(
    item_id: &str,
    desired: &Desired,
    changes: &[Change],
    provider_marker: &str,
) -> Result<(), String> {
    let (base, token) = config_async().await?;

    let mut body = serde_json::json!({});
    for change in changes {
        match change {
            Change::Duration { to, .. } => body["duration"] = serde_json::json!({ "minutes": to }),
            Change::Date { to, .. } => body["date"] = serde_json::json!(to),
            Change::Type { to, .. } => body["type"] = serde_json::json!({ "id": to }),
            Change::Text => body["text"] = serde_json::json!(compose_text(&desired.text, provider_marker)),
        }
    }

    send_checked(
        reqwest::Client::new()
            .post(format!(
                "{}/api/issues/{}/timeTracking/workItems/{}?fields=id",
                base, desired.issue, item_id
            ))
            .headers(auth_headers(&token)?)
            .json(&body),
    )
    .await
    .map(|_| ())
}

pub async fn delete_work_item(issue: &str, item_id: &str) -> Result<(), String> {
    let (base, token) = config_async().await?;
    send_checked(
        reqwest::Client::new()
            .delete(format!("{}/api/issues/{}/timeTracking/workItems/{}", base, issue, item_id))
            .headers(auth_headers(&token)?),
    )
    .await
    .map(|_| ())
}
