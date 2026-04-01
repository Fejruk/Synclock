use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::Mutex;

const BASE_URL: &str = "https://api.timeular.com/api/v3";

static TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn token_mutex() -> &'static Mutex<Option<String>> {
    TOKEN.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Deserialize)]
struct SignInResponse {
    token: String,
}

pub async fn clear_token_cache() {
    *token_mutex().lock().await = None;
}

async fn get_token() -> Result<String, String> {
    let mut cached = token_mutex().lock().await;
    if let Some(ref t) = *cached {
        return Ok(t.clone());
    }

    let api_key = std::env::var("EARLY_API_KEY")
        .map_err(|_| "EARLY_API_KEY not set".to_string())?;
    let api_secret = std::env::var("EARLY_API_SECRET")
        .map_err(|_| "EARLY_API_SECRET not set".to_string())?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/developer/sign-in", BASE_URL))
        .json(&serde_json::json!({
            "apiKey": api_key,
            "apiSecret": api_secret,
        }))
        .send()
        .await
        .map_err(|e| format!("Early auth request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Early auth failed ({}): {}", status, body));
    }

    let data: SignInResponse = resp.json().await.map_err(|e| e.to_string())?;
    *cached = Some(data.token.clone());
    Ok(data.token)
}

async fn early_fetch(path: &str) -> Result<serde_json::Value, String> {
    let token = get_token().await?;
    let client = reqwest::Client::new();

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse().unwrap());

    let resp = client
        .get(format!("{}{}", BASE_URL, path))
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("Early API error: {}", e))?;

    if resp.status().as_u16() == 401 {
        // Clear token and retry
        *token_mutex().lock().await = None;
        let token2 = get_token().await?;
        let mut headers2 = HeaderMap::new();
        headers2.insert(AUTHORIZATION, format!("Bearer {}", token2).parse().unwrap());

        let resp2 = client
            .get(format!("{}{}", BASE_URL, path))
            .headers(headers2)
            .send()
            .await
            .map_err(|e| format!("Early API error: {}", e))?;

        if !resp2.status().is_success() {
            return Err(format!("Early API error ({})", resp2.status()));
        }
        return resp2.json().await.map_err(|e| e.to_string());
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Early API error ({}): {}", status, body));
    }

    resp.json().await.map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Duration {
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "stoppedAt")]
    pub stopped_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoteMention {
    pub id: i64,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoteTag {
    pub id: i64,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Note {
    pub text: Option<String>,
    #[serde(default)]
    pub tags: Vec<NoteTag>,
    #[serde(default)]
    pub mentions: Vec<NoteMention>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TimeEntry {
    pub id: String,
    #[serde(rename = "activityId")]
    pub activity_id: String,
    pub duration: Duration,
    pub note: Option<Note>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Activity {
    pub id: String,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Deserialize)]
struct TimeEntriesResponse {
    #[serde(rename = "timeEntries", default)]
    time_entries: Vec<TimeEntry>,
}

#[derive(Debug, Deserialize)]
struct ActivitiesResponse {
    #[serde(default)]
    activities: Vec<Activity>,
}

pub async fn get_time_entries(from: &str, to: &str) -> Result<Vec<TimeEntry>, String> {
    let data = early_fetch(&format!(
        "/time-entries/{}T00:00:00.000/{}T23:59:59.999",
        from, to
    )).await?;

    let resp: TimeEntriesResponse = serde_json::from_value(data).map_err(|e| e.to_string())?;
    Ok(resp.time_entries)
}

pub async fn get_activities() -> Result<Vec<Activity>, String> {
    let data = early_fetch("/activities").await?;
    let resp: ActivitiesResponse = serde_json::from_value(data).map_err(|e| e.to_string())?;
    Ok(resp.activities)
}

pub fn extract_jira_keys(entry: &TimeEntry) -> Vec<String> {
    let mut keys = std::collections::HashSet::new();
    let re = regex_lite::Regex::new(r"\b([A-Z][A-Z0-9]+-\d+)\b").unwrap();

    if let Some(note) = &entry.note {
        for mention in &note.mentions {
            if let Some(label) = &mention.label {
                if let Some(m) = re.find(label) {
                    keys.insert(m.as_str().to_string());
                }
            }
        }
        for tag in &note.tags {
            if let Some(label) = &tag.label {
                if let Some(m) = re.find(label) {
                    keys.insert(m.as_str().to_string());
                }
            }
        }
        if let Some(text) = &note.text {
            for m in re.find_iter(text) {
                keys.insert(m.as_str().to_string());
            }
        }
    }

    keys.into_iter().collect()
}

pub fn get_duration_minutes(entry: &TimeEntry) -> i64 {
    let start = chrono::NaiveDateTime::parse_from_str(&entry.duration.started_at, "%Y-%m-%dT%H:%M:%S%.3f");
    let stop = chrono::NaiveDateTime::parse_from_str(&entry.duration.stopped_at, "%Y-%m-%dT%H:%M:%S%.3f");

    match (start, stop) {
        (Ok(s), Ok(e)) => (e - s).num_minutes(),
        _ => 0,
    }
}

pub fn clean_note_text(text: &str) -> String {
    let re = regex_lite::Regex::new(r"<\{\{?\|[mt]\|\d+\|\}?\}>").unwrap();
    re.replace_all(text, "").trim().to_string()
}
