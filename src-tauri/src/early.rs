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

/// Convert a local calendar date (`YYYY-MM-DD`) plus a wall-clock time into the
/// timestamp string Timeular expects. Timeular filters and stores in UTC, so the
/// user's local day boundaries must be translated to UTC — otherwise entries near
/// local midnight (e.g. 01:00 in CEST = 23:00 UTC the previous day) get grouped
/// under the wrong calendar day in the panel.
fn local_datetime_to_utc_string(date: &str, h: u32, m: u32, s: u32, ms: u32) -> Result<String, String> {
    use chrono::{Duration, Local, LocalResult, NaiveDate, TimeZone, Utc};
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| format!("Invalid date '{}': {}", date, e))?;
    let naive = day
        .and_hms_milli_opt(h, m, s, ms)
        .ok_or_else(|| format!("Invalid time for date '{}'", date))?;
    let local_dt = match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(dt, _) => dt, // DST fall-back: take the earlier instant
        LocalResult::None => {
            // DST spring-forward gap (rare at day boundaries): nudge past it.
            Local
                .from_local_datetime(&(naive + Duration::hours(1)))
                .single()
                .ok_or_else(|| format!("Unresolvable local time for date '{}'", date))?
        }
    };
    Ok(local_dt
        .with_timezone(&Utc)
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string())
}

/// Parse a naive UTC timestamp (the format Timeular/Early use, no offset) into
/// milliseconds since the epoch. Accepts both with- and without-millisecond forms.
fn naive_utc_ms(s: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
        .ok()
        .map(|dt| dt.and_utc().timestamp_millis())
}

pub async fn get_time_entries(from: &str, to: &str) -> Result<Vec<TimeEntry>, String> {
    let start = local_datetime_to_utc_string(from, 0, 0, 0, 0)?;
    let end = local_datetime_to_utc_string(to, 23, 59, 59, 999)?;
    let data = early_fetch(&format!("/time-entries/{}/{}", start, end)).await?;

    let resp: TimeEntriesResponse = serde_json::from_value(data).map_err(|e| e.to_string())?;

    // Timeular returns every entry that *overlaps* the window, so an entry that
    // started the previous local day but ran past midnight comes back too. Keep
    // only entries that actually *started* on/after the range start, so a
    // midnight-spanning entry appears on its start day rather than on both days.
    // (Unparseable timestamps are kept rather than silently dropped.)
    let start_ms = naive_utc_ms(&start).unwrap_or(i64::MIN);
    Ok(resp
        .time_entries
        .into_iter()
        .filter(|e| naive_utc_ms(&e.duration.started_at).map_or(true, |ms| ms >= start_ms))
        .collect())
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

    // ── local_datetime_to_utc_string ──

    fn parse_utc_ms(s: &str) -> i64 {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f")
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn day_bounds_span_full_day() {
        // 2026-05-26 has no DST transition in any common zone, so a local
        // [00:00:00.000 .. 23:59:59.999] window is exactly 86_399_999 ms wide
        // regardless of the machine's timezone offset.
        let start = local_datetime_to_utc_string("2026-05-26", 0, 0, 0, 0).unwrap();
        let end = local_datetime_to_utc_string("2026-05-26", 23, 59, 59, 999).unwrap();
        assert_eq!(parse_utc_ms(&end) - parse_utc_ms(&start), 86_399_999);
    }

    #[test]
    fn produces_parseable_utc_format() {
        let s = local_datetime_to_utc_string("2026-01-15", 0, 0, 0, 0).unwrap();
        // Must round-trip through the same naive-UTC format the rest of the code uses.
        assert!(chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.3f").is_ok());
    }

    #[test]
    fn rejects_bad_date() {
        assert!(local_datetime_to_utc_string("not-a-date", 0, 0, 0, 0).is_err());
    }

    // ── naive_utc_ms ──

    #[test]
    fn parses_millis_form() {
        assert_eq!(naive_utc_ms("2026-05-26T23:00:00.000"), Some(1779836400000));
    }

    #[test]
    fn parses_without_millis() {
        assert_eq!(
            naive_utc_ms("2026-05-26T23:00:00"),
            naive_utc_ms("2026-05-26T23:00:00.000")
        );
    }

    #[test]
    fn naive_utc_ms_rejects_garbage() {
        assert_eq!(naive_utc_ms("nope"), None);
    }
}
