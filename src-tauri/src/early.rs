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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(note_text: Option<&str>, mentions: Vec<(&str, i64)>, tags: Vec<(&str, i64)>) -> TimeEntry {
        TimeEntry {
            id: "1".into(),
            activity_id: "100".into(),
            duration: Duration {
                started_at: "2026-04-01T07:00:00.000".into(),
                stopped_at: "2026-04-01T07:30:00.000".into(),
            },
            note: Some(Note {
                text: note_text.map(|s| s.into()),
                mentions: mentions.into_iter().map(|(label, id)| NoteMention { id, label: Some(label.into()) }).collect(),
                tags: tags.into_iter().map(|(label, id)| NoteTag { id, label: Some(label.into()) }).collect(),
            }),
        }
    }

    // ── extract_jira_keys ──

    #[test]
    fn extract_keys_from_mentions() {
        let entry = make_entry(None, vec![("SYNC-42", 1)], vec![]);
        let keys = extract_jira_keys(&entry);
        assert_eq!(keys, vec!["SYNC-42"]);
    }

    #[test]
    fn extract_keys_from_tags() {
        let entry = make_entry(None, vec![], vec![("WEB-789", 1)]);
        let keys = extract_jira_keys(&entry);
        assert_eq!(keys, vec!["WEB-789"]);
    }

    #[test]
    fn extract_keys_from_note_text() {
        let entry = make_entry(Some("Working on CORE-10 and API-55"), vec![], vec![]);
        let mut keys = extract_jira_keys(&entry);
        keys.sort();
        assert_eq!(keys, vec!["API-55", "CORE-10"]);
    }

    #[test]
    fn extract_keys_deduplicates() {
        let entry = make_entry(Some("SYNC-42 stuff"), vec![("SYNC-42", 1)], vec![]);
        let keys = extract_jira_keys(&entry);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], "SYNC-42");
    }

    #[test]
    fn extract_keys_no_match() {
        let entry = make_entry(Some("no jira keys here"), vec![], vec![]);
        let keys = extract_jira_keys(&entry);
        assert!(keys.is_empty());
    }

    #[test]
    fn extract_keys_no_note() {
        let entry = TimeEntry {
            id: "1".into(),
            activity_id: "100".into(),
            duration: Duration { started_at: "2026-04-01T07:00:00.000".into(), stopped_at: "2026-04-01T07:30:00.000".into() },
            note: None,
        };
        assert!(extract_jira_keys(&entry).is_empty());
    }

    #[test]
    fn extract_keys_ignores_lowercase() {
        let entry = make_entry(Some("is-123 not a key"), vec![], vec![]);
        assert!(extract_jira_keys(&entry).is_empty());
    }

    // ── get_duration_minutes ──

    #[test]
    fn duration_30_minutes() {
        let entry = make_entry(None, vec![], vec![]);
        assert_eq!(get_duration_minutes(&entry), 30);
    }

    #[test]
    fn duration_2_hours() {
        let entry = TimeEntry {
            id: "1".into(),
            activity_id: "100".into(),
            duration: Duration {
                started_at: "2026-04-01T07:00:00.000".into(),
                stopped_at: "2026-04-01T09:00:00.000".into(),
            },
            note: None,
        };
        assert_eq!(get_duration_minutes(&entry), 120);
    }

    #[test]
    fn duration_invalid_returns_zero() {
        let entry = TimeEntry {
            id: "1".into(),
            activity_id: "100".into(),
            duration: Duration { started_at: "invalid".into(), stopped_at: "invalid".into() },
            note: None,
        };
        assert_eq!(get_duration_minutes(&entry), 0);
    }

    // ── clean_note_text ──

    #[test]
    fn clean_removes_mention_markup() {
        assert_eq!(clean_note_text("<{{|m|487500|}}>"), "");
    }

    #[test]
    fn clean_removes_tag_markup() {
        assert_eq!(clean_note_text("<{|t|123|}>"), "");
    }

    #[test]
    fn clean_preserves_surrounding_text() {
        assert_eq!(clean_note_text("Hello <{{|m|487500|}}> world"), "Hello  world");
    }

    #[test]
    fn clean_no_markup() {
        assert_eq!(clean_note_text("just plain text"), "just plain text");
    }

    #[test]
    fn clean_empty_string() {
        assert_eq!(clean_note_text(""), "");
    }

    #[test]
    fn clean_multiple_markups() {
        assert_eq!(
            clean_note_text("<{{|m|100|}}> and <{{|m|200|}}>"),
            "and"
        );
    }
}
