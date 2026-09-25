mod config;
mod early;
mod jira;
mod reconcile;
mod toggl;
mod youtrack;

use chrono::NaiveDate;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconEvent,
    Emitter, Manager,
};

// ── Shared types ──

#[derive(Clone, Serialize)]
struct PreviewItem {
    id: String,
    activity: String,
    activity_color: String,
    jira_keys: Vec<String>,
    duration_min: i64,
    started_at: String,
    stopped_at: String,
    note: String,
    has_jira_key: bool,
    synced: bool,
    /// What a sync would do for this entry, one op per affected work item.
    ops: Vec<PreviewOp>,
}

#[derive(Clone, Serialize)]
struct PreviewOp {
    entry_id: String,
    /// Local day (`YYYY-MM-DD`) of the work item the op touches.
    day: String,
    action: String, // "ok" | "create" | "update" | "delete" | "blocked"
    issue: String,
    minutes: i64,
    detail: String,
}

#[derive(Clone, Serialize, Default)]
struct PlanSummary {
    create: usize,
    update: usize,
    delete: usize,
    ok: usize,
    blocked: usize,
    /// Minutes the target is missing (creates, lengthened items).
    minutes_missing: i64,
    /// Minutes the target has too many (deletes, shortened items).
    minutes_extra: i64,
    /// Deletions would wait for confirmation (see `deletes_need_confirmation`).
    deletes_need_confirm: bool,
    /// Reconciled window (`YYYY-MM-DD`), empty for Jira.
    window_from: String,
    window_to: String,
}

#[derive(Clone, Serialize)]
struct PreviewResponse {
    total: usize,
    with_jira: usize,
    items: Vec<PreviewItem>,
    /// Ops outside the requested days: other days of the window and orphans.
    other_ops: Vec<PreviewOp>,
    summary: PlanSummary,
}

#[derive(Clone, Serialize)]
struct SyncResultItem {
    entry_id: String,
    activity: String,
    issue_key: String,
    action: String,
    duration: String,
    success: bool,
    skipped: bool,
    error: Option<String>,
}

#[derive(Clone, Serialize, Default)]
struct SyncResponse {
    /// Work items changed in the target (created + updated + deleted).
    synced: usize,
    created: usize,
    updated: usize,
    deleted: usize,
    skipped: usize,
    failed: usize,
    /// Deletions held back until confirmed.
    deletes_pending: usize,
    /// The held-back deletions; confirming passes their `item_id`s back.
    pending_deletes: Vec<PendingDelete>,
    results: Vec<SyncResultItem>,
}

#[derive(Clone, Serialize)]
struct PendingDelete {
    item_id: String,
    activity: String,
    issue: String,
    day: String,
    minutes: i64,
    reason: String,
}

// ── Unified provider interface ──

struct TimeEntry {
    id: String,
    activity: String,
    activity_id: Option<String>,
    activity_color: String,
    jira_keys: Vec<String>,
    duration_min: i64,
    started_at: String,
    stopped_at: String,
    note: String,
}

async fn fetch_entries(from: &str, to: &str) -> Result<Vec<TimeEntry>, String> {
    let cfg = config::get_config().await;
    let mut entries = match cfg.provider.as_str() {
        "toggl" => fetch_toggl_entries(from, to).await,
        _ => fetch_early_entries(from, to).await,
    }?;

    // Entries with no issue key detected in the time tracker fall back to the
    // configured default task (if any). Applied here so both preview and sync
    // see the same keys — the preview then shows where untagged time will land.
    let default_key = cfg.default_issue_key.trim();
    if !default_key.is_empty() {
        for e in &mut entries {
            if e.jira_keys.is_empty() {
                e.jira_keys.push(default_key.to_string());
            }
        }
    }

    Ok(entries)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target { Jira, YouTrack }

fn current_target(cfg: &config::AppConfig) -> Target {
    match cfg.target.as_str() {
        "youtrack" => Target::YouTrack,
        _ => Target::Jira,
    }
}

fn dedup_marker_for(provider: &str, entry_id: &str) -> String {
    format!("{}-{}", provider, entry_id)
}

async fn target_test_connection(target: Target) -> Result<(), String> {
    match target {
        Target::Jira => jira::test_connection().await,
        Target::YouTrack => youtrack::test_connection().await,
    }
}

async fn fetch_early_entries(from: &str, to: &str) -> Result<Vec<TimeEntry>, String> {
    let (entries, activities) = tokio::try_join!(
        early::get_time_entries(from, to),
        early::get_activities(),
    )?;

    let act_map: std::collections::HashMap<String, &early::Activity> =
        activities.iter().map(|a| (a.id.clone(), a)).collect();

    Ok(entries.iter().map(|e| {
        let act = act_map.get(&e.activity_id);
        TimeEntry {
            id: e.id.clone(),
            activity: act.map(|a| a.name.clone()).unwrap_or_else(|| "Unknown".into()),
            activity_id: Some(e.activity_id.clone()),
            activity_color: act.map(|a| a.color.clone()).unwrap_or_else(|| "888".into()),
            jira_keys: early::extract_jira_keys(e),
            duration_min: early::get_duration_minutes(e),
            started_at: e.duration.started_at.clone(),
            stopped_at: e.duration.stopped_at.clone(),
            note: early::clean_note_text(e.note.as_ref().and_then(|n| n.text.as_deref()).unwrap_or("")),
        }
    }).collect())
}

async fn fetch_toggl_entries(from: &str, to: &str) -> Result<Vec<TimeEntry>, String> {
    let entries = toggl::get_time_entries(from, to).await?;

    Ok(entries.iter().map(|e| {
        let dur_min = e.duration / 60;
        let start = &e.start;
        let stop = e.stop.as_deref().unwrap_or(start);
        TimeEntry {
            id: e.id.to_string(),
            activity: e.description.clone().unwrap_or_else(|| "No description".into()),
            activity_id: None,
            activity_color: "6366f1".into(), // purple for Toggl
            jira_keys: toggl::extract_jira_keys(e),
            duration_min: dur_min,
            started_at: start.clone(),
            stopped_at: stop.to_string(),
            note: String::new(),
        }
    }).collect())
}

fn fmt_duration(min: i64) -> String {
    let h = min / 60;
    let m = min % 60;
    match (h, m) {
        (0, m) => format!("{}m", m),
        (h, 0) => format!("{}h", h),
        (h, m) => format!("{}h {}m", h, m),
    }
}

fn per_key_minutes(e: &TimeEntry) -> i64 {
    (e.duration_min as f64 / e.jira_keys.len() as f64).round().max(1.0) as i64
}

fn entry_comment(e: &TimeEntry) -> String {
    if e.note.is_empty() { e.activity.clone() } else { format!("{} - {}", e.activity, e.note) }
}

fn parse_day(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| format!("Invalid date '{}': {}", s, e))
}

fn day_ms(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis()
}

/// Local calendar day of a work item date (stored as UTC midnight of that day).
fn ms_day(ms: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.date_naive()).unwrap_or_default()
}

fn entry_day(e: &TimeEntry) -> NaiveDate {
    ms_day(youtrack::started_to_local_day_ms(&e.started_at))
}

// ── YouTrack reconciliation ──

/// Extra days fetched on both sides of the window so entries moved across its
/// edge still find their work items (and vice versa).
const BUFFER_DAYS: i64 = 31;

struct YtPlan {
    entries: Vec<TimeEntry>,
    ops: Vec<reconcile::Op>,
    window: (NaiveDate, NaiveDate),
    entries_in_window: usize,
}

/// The window always covers the last `window_days` days and the requested range.
fn reconcile_window(from: &str, to: &str, window_days: u32) -> Result<(NaiveDate, NaiveDate), String> {
    let today = chrono::Local::now().date_naive();
    let back = chrono::Duration::days(window_days.clamp(1, 90) as i64 - 1);
    Ok((parse_day(from)?.min(today - back), parse_day(to)?.max(today)))
}

async fn build_yt_plan(cfg: &config::AppConfig, from: &str, to: &str) -> Result<YtPlan, String> {
    let window = reconcile_window(from, to, cfg.sync_window_days)?;
    let buffer = chrono::Duration::days(BUFFER_DAYS);
    let fetch_from = (window.0 - buffer).format("%Y-%m-%d").to_string();
    let fetch_to = (window.1 + buffer).format("%Y-%m-%d").to_string();

    // Both must succeed: planning against a partial picture would delete items
    // whose entries simply weren't loaded.
    let (entries, items) = tokio::try_join!(
        fetch_entries(&fetch_from, &fetch_to),
        youtrack::get_my_work_items(&fetch_from, &fetch_to),
    )?;

    let existing: Vec<reconcile::Existing> = items
        .into_iter()
        .filter_map(|w| {
            let entry_id = reconcile::parse_entry_id(&w.text, &cfg.provider)?;
            let issue = w.issue.map(|i| i.id_readable).filter(|i| !i.is_empty())?;
            Some(reconcile::Existing {
                id: w.id,
                entry_id,
                issue,
                minutes: w.duration.minutes,
                date_ms: w.date,
                type_id: w.item_type.map(|t| t.id).filter(|t| !t.is_empty()),
                text: w.text,
            })
        })
        .collect();
    let with_items: HashSet<&str> = existing.iter().map(|x| x.entry_id.as_str()).collect();

    let in_window = |e: &TimeEntry| {
        let d = entry_day(e);
        d >= window.0 && d <= window.1
    };

    // Resolve keys (legacy aliases → YouTrack ids) only for entries that get planned.
    let mut resolved: HashMap<String, Result<String, String>> = HashMap::new();
    for e in entries.iter().filter(|e| in_window(e) || with_items.contains(e.id.as_str())) {
        for k in &e.jira_keys {
            if !resolved.contains_key(k) {
                resolved.insert(k.clone(), youtrack::resolve_issue_id(k).await);
            }
        }
    }

    let inputs: Vec<reconcile::EntryInput> = entries
        .iter()
        .map(|e| {
            let targets = if e.jira_keys.is_empty() || e.duration_min < 1 {
                Ok(Vec::new())
            } else {
                let per_key = per_key_minutes(e);
                let type_id = e.activity_id.as_ref()
                    .and_then(|aid| cfg.activity_type_map.get(aid))
                    .filter(|s| !s.is_empty())
                    .cloned();
                e.jira_keys
                    .iter()
                    .map(|k| {
                        let issue = resolved.get(k).cloned().unwrap_or_else(|| Err(format!("{} not resolved", k)))?;
                        Ok(reconcile::Desired {
                            issue,
                            minutes: per_key,
                            date_ms: youtrack::started_to_local_day_ms(&e.started_at),
                            type_id: type_id.clone(),
                            text: entry_comment(e),
                        })
                    })
                    .collect()
            };
            reconcile::EntryInput { entry_id: e.id.clone(), in_window: in_window(e), targets }
        })
        .collect();

    let ops = reconcile::plan(&inputs, &existing, (day_ms(window.0), day_ms(window.1)));
    let entries_in_window = entries.iter().filter(|e| in_window(e)).count();
    Ok(YtPlan { entries, ops, window, entries_in_window })
}

fn fmt_day(ms: i64) -> String {
    ms_day(ms).format("%-d.%-m.").to_string()
}

fn describe_change(c: &reconcile::Change) -> String {
    use reconcile::Change;
    match c {
        Change::Duration { from, to } => format!("{} → {}", fmt_duration(*from), fmt_duration(*to)),
        Change::Date { from, to } => format!("day {} → {}", fmt_day(*from), fmt_day(*to)),
        Change::Type { .. } => "type".into(),
        Change::Text => "text".into(),
    }
}

fn preview_op(op: &reconcile::Op) -> PreviewOp {
    use reconcile::{DeleteReason, Op};
    let (action, issue, minutes, date_ms, detail) = match op {
        Op::Keep { issue, minutes, .. } => ("ok", issue.clone(), *minutes, None, String::new()),
        Op::Create { desired, .. } => ("create", desired.issue.clone(), desired.minutes, Some(desired.date_ms), String::new()),
        Op::Update { desired, changes, .. } => (
            "update",
            desired.issue.clone(),
            desired.minutes,
            Some(desired.date_ms),
            changes.iter().map(describe_change).collect::<Vec<_>>().join(", "),
        ),
        Op::Delete { issue, minutes, date_ms, reason, .. } => (
            "delete",
            issue.clone(),
            *minutes,
            Some(*date_ms),
            match reason {
                DeleteReason::Orphan => "entry deleted",
                DeleteReason::NotTarget => "issue changed",
                DeleteReason::Duplicate => "duplicate",
            }
            .into(),
        ),
        Op::Blocked { error, .. } => ("blocked", String::new(), 0, None, error.clone()),
    };
    PreviewOp {
        entry_id: op.entry_id().to_string(),
        day: date_ms.map(|ms| ms_day(ms).format("%Y-%m-%d").to_string()).unwrap_or_default(),
        action: action.into(),
        issue,
        minutes,
        detail,
    }
}

fn summarize(ops: &[reconcile::Op]) -> PlanSummary {
    use reconcile::{Change, Op};
    let mut s = PlanSummary::default();
    for op in ops {
        match op {
            Op::Keep { .. } => s.ok += 1,
            Op::Create { desired, .. } => {
                s.create += 1;
                s.minutes_missing += desired.minutes;
            }
            Op::Update { changes, .. } => {
                s.update += 1;
                for c in changes {
                    if let Change::Duration { from, to } = c {
                        if to > from { s.minutes_missing += to - from } else { s.minutes_extra += from - to }
                    }
                }
            }
            Op::Delete { minutes, .. } => {
                s.delete += 1;
                s.minutes_extra += minutes;
            }
            Op::Blocked { .. } => s.blocked += 1,
        }
    }
    s
}

fn to_preview_item(e: &TimeEntry, ops: Vec<PreviewOp>) -> PreviewItem {
    PreviewItem {
        id: e.id.clone(),
        activity: e.activity.clone(),
        activity_color: e.activity_color.clone(),
        jira_keys: e.jira_keys.clone(),
        duration_min: e.duration_min,
        started_at: e.started_at.clone(),
        stopped_at: e.stopped_at.clone(),
        note: e.note.clone(),
        has_jira_key: !e.jira_keys.is_empty(),
        synced: !e.jira_keys.is_empty() && !ops.is_empty() && ops.iter().all(|o| o.action == "ok"),
        ops,
    }
}

async fn preview_youtrack(cfg: &config::AppConfig, from: &str, to: &str) -> Result<PreviewResponse, String> {
    let plan = build_yt_plan(cfg, from, to).await?;
    let (shown_from, shown_to) = (parse_day(from)?, parse_day(to)?);
    let shown = |e: &TimeEntry| {
        let d = entry_day(e);
        d >= shown_from && d <= shown_to
    };

    let mut summary = summarize(&plan.ops);
    summary.deletes_need_confirm = reconcile::deletes_need_confirmation(
        summary.delete, plan.entries_in_window, cfg.max_deletes_without_confirm,
    );
    summary.window_from = plan.window.0.format("%Y-%m-%d").to_string();
    summary.window_to = plan.window.1.format("%Y-%m-%d").to_string();

    let shown_ids: HashSet<&str> = plan.entries.iter().filter(|e| shown(e)).map(|e| e.id.as_str()).collect();
    let mut by_entry: HashMap<&str, Vec<PreviewOp>> = HashMap::new();
    let mut other_ops = Vec::new();
    for op in &plan.ops {
        if shown_ids.contains(op.entry_id()) {
            by_entry.entry(op.entry_id()).or_default().push(preview_op(op));
        } else if !matches!(op, reconcile::Op::Keep { .. }) {
            other_ops.push(preview_op(op));
        }
    }
    other_ops.sort_by(|a, b| a.day.cmp(&b.day));

    let items: Vec<PreviewItem> = plan.entries.iter()
        .filter(|e| shown(e))
        .map(|e| to_preview_item(e, by_entry.remove(e.id.as_str()).unwrap_or_default()))
        .collect();
    let with_jira = items.iter().filter(|i| i.has_jira_key).count();
    Ok(PreviewResponse { total: items.len(), with_jira, items, other_ops, summary })
}

/// Background, tray and panel syncs may overlap; running two plans at once
/// would create the same work items twice.
static YT_SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `confirmed` lists item ids the user approved for deletion after they were
/// held back. Only those are deleted — never whatever a fresh plan would add.
async fn sync_youtrack(cfg: &config::AppConfig, from: &str, to: &str, confirmed: Option<Vec<String>>) -> Result<SyncResponse, String> {
    use reconcile::Op;
    let _guard = YT_SYNC_LOCK.lock().await;
    let plan = build_yt_plan(cfg, from, to).await?;
    let activity: HashMap<&str, &str> = plan.entries.iter().map(|e| (e.id.as_str(), e.activity.as_str())).collect();
    let deletes = plan.ops.iter().filter(|o| matches!(o, Op::Delete { .. })).count();
    let unattended = !reconcile::deletes_need_confirmation(deletes, plan.entries_in_window, cfg.max_deletes_without_confirm);
    let confirmed: HashSet<String> = confirmed.unwrap_or_default().into_iter().collect();
    let may_delete = |item_id: &str| unattended || confirmed.contains(item_id);

    let mut resp = SyncResponse::default();
    let result = |op: &Op, issue: &str, action: &str, duration: String, outcome: &Result<(), String>| SyncResultItem {
        entry_id: op.entry_id().to_string(),
        activity: activity.get(op.entry_id()).copied().unwrap_or("Deleted entry").to_string(),
        issue_key: issue.to_string(),
        action: action.into(),
        duration,
        success: outcome.is_ok(),
        skipped: false,
        error: outcome.as_ref().err().cloned(),
    };

    // Create → update → delete: if something fails midway, time is rather
    // logged twice for a moment than missing.
    for op in plan.ops.iter().filter(|o| matches!(o, Op::Create { .. })) {
        if let Op::Create { desired, .. } = op {
            let marker = dedup_marker_for(&cfg.provider, op.entry_id());
            let outcome = youtrack::create_work_item(desired, &marker).await.map(|_| ());
            if outcome.is_ok() { resp.created += 1 }
            resp.results.push(result(op, &desired.issue, "create", fmt_duration(desired.minutes), &outcome));
        }
    }
    for op in plan.ops.iter().filter(|o| matches!(o, Op::Update { .. })) {
        if let Op::Update { item_id, desired, changes, .. } = op {
            let marker = dedup_marker_for(&cfg.provider, op.entry_id());
            let outcome = youtrack::update_work_item(item_id, desired, changes, &marker).await;
            if outcome.is_ok() { resp.updated += 1 }
            let detail = changes.iter().map(describe_change).collect::<Vec<_>>().join(", ");
            resp.results.push(result(op, &desired.issue, "update", detail, &outcome));
        }
    }
    for op in plan.ops.iter().filter(|o| matches!(o, Op::Delete { .. })) {
        if let Op::Delete { item_id, issue, minutes, .. } = op {
            if may_delete(item_id) {
                let outcome = youtrack::delete_work_item(issue, item_id).await;
                if outcome.is_ok() { resp.deleted += 1 }
                resp.results.push(result(op, issue, "delete", fmt_duration(*minutes), &outcome));
            } else {
                let p = preview_op(op);
                resp.pending_deletes.push(PendingDelete {
                    item_id: item_id.clone(),
                    activity: activity.get(op.entry_id()).copied().unwrap_or("Deleted entry").to_string(),
                    issue: issue.clone(),
                    day: p.day,
                    minutes: *minutes,
                    reason: p.detail,
                });
            }
        }
    }
    resp.deletes_pending = resp.pending_deletes.len();
    for op in &plan.ops {
        match op {
            // Unchanged items span the whole window (hundreds) — count, don't list.
            Op::Keep { .. } => resp.skipped += 1,
            Op::Blocked { error, .. } => {
                resp.results.push(result(op, "", "blocked", String::new(), &Err(error.clone())));
            }
            _ => {}
        }
    }

    resp.synced = resp.created + resp.updated + resp.deleted;
    resp.failed = resp.results.iter().filter(|r| !r.success).count();
    Ok(resp)
}

// ── Jira (create-only, matched by start time and duration) ──

async fn preview_jira(from: &str, to: &str) -> Result<PreviewResponse, String> {
    let entries = fetch_entries(from, to).await?;

    let mut jira_map: HashMap<String, Vec<jira::Worklog>> = HashMap::new();
    for e in &entries {
        for k in &e.jira_keys {
            if !jira_map.contains_key(k) {
                jira_map.insert(k.clone(), jira::get_worklogs(k).await.unwrap_or_default());
            }
        }
    }

    let mut summary = PlanSummary::default();
    let items: Vec<PreviewItem> = entries.iter().map(|e| {
        let ops = if e.jira_keys.is_empty() || e.duration_min < 1 { Vec::new() } else {
            let per_key = per_key_minutes(e);
            e.jira_keys.iter().map(|k| {
                let wls = jira_map.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
                let done = jira::is_already_synced(wls, &e.started_at, per_key);
                if done { summary.ok += 1 } else { summary.create += 1; summary.minutes_missing += per_key }
                PreviewOp {
                    entry_id: e.id.clone(),
                    day: entry_day(e).format("%Y-%m-%d").to_string(),
                    action: if done { "ok" } else { "create" }.into(),
                    issue: k.clone(),
                    minutes: per_key,
                    detail: String::new(),
                }
            }).collect()
        };
        to_preview_item(e, ops)
    }).collect();

    let with_jira = items.iter().filter(|i| i.has_jira_key).count();
    Ok(PreviewResponse { total: items.len(), with_jira, items, other_ops: Vec::new(), summary })
}

async fn sync_jira(from: &str, to: &str) -> Result<SyncResponse, String> {
    let entries = fetch_entries(from, to).await?;
    let mut resp = SyncResponse::default();

    for e in &entries {
        if e.jira_keys.is_empty() || e.duration_min < 1 { continue; }

        let per_key = per_key_minutes(e);
        let comment = entry_comment(e);

        for key in &e.jira_keys {
            let wls = jira::get_worklogs(key).await.unwrap_or_default();
            let mut r = SyncResultItem {
                entry_id: e.id.clone(), activity: e.activity.clone(),
                issue_key: key.clone(), action: "ok".into(), duration: String::new(),
                success: true, skipped: false, error: None,
            };
            if jira::is_already_synced(&wls, &e.started_at, per_key) {
                r.skipped = true;
                resp.skipped += 1;
            } else {
                r.action = "create".into();
                match jira::add_worklog(key, per_key, &e.started_at, &comment).await {
                    Ok(_) => { r.duration = fmt_duration(per_key); resp.created += 1; }
                    Err(err) => { r.success = false; r.error = Some(err); }
                }
            }
            resp.results.push(r);
        }
    }

    resp.synced = resp.created;
    resp.failed = resp.results.iter().filter(|r| !r.success).count();
    Ok(resp)
}

// ── Commands ──

#[tauri::command]
async fn check_status() -> serde_json::Value {
    let cfg = config::get_config().await;
    let provider_ok = match cfg.provider.as_str() {
        "toggl" => toggl::test_connection().await.is_ok(),
        _ => early::get_activities().await.is_ok(),
    };
    let target = current_target(&cfg);
    let target_check = match target_test_connection(target).await {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };

    serde_json::json!({
        "provider": cfg.provider,
        "provider_ok": provider_ok,
        "target": cfg.target,
        "target_check": target_check,
        "configured": cfg.is_configured(),
    })
}

#[tauri::command]
async fn preview(from: String, to: String) -> Result<PreviewResponse, String> {
    let cfg = config::get_config().await;
    match current_target(&cfg) {
        Target::Jira => preview_jira(&from, &to).await,
        Target::YouTrack => preview_youtrack(&cfg, &from, &to).await,
    }
}

/// For YouTrack, `from..to` is widened to the reconciliation window. Deletions
/// that need confirmation are held back unless their ids are in `confirmed_deletes`.
#[tauri::command]
async fn sync(from: String, to: String, confirmed_deletes: Option<Vec<String>>) -> Result<SyncResponse, String> {
    let cfg = config::get_config().await;
    match current_target(&cfg) {
        Target::Jira => sync_jira(&from, &to).await,
        Target::YouTrack => sync_youtrack(&cfg, &from, &to, confirmed_deletes).await,
    }
}

#[tauri::command]
async fn get_settings() -> config::AppConfig {
    config::get_config().await
}

#[derive(Clone, Serialize)]
struct ActivityOption {
    id: String,
    name: String,
    color: String,
}

#[tauri::command]
async fn get_early_activities() -> Result<Vec<ActivityOption>, String> {
    let acts = early::get_activities().await?;
    let mut out: Vec<ActivityOption> = acts
        .iter()
        .map(|a| ActivityOption {
            id: a.id.clone(),
            name: a.name.clone(),
            color: a.color.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

#[tauri::command]
async fn get_youtrack_work_item_types() -> Result<Vec<youtrack::WorkItemType>, String> {
    youtrack::get_work_item_types().await
}

#[tauri::command]
async fn save_settings(app: tauri::AppHandle, settings: config::AppConfig) -> Result<(), String> {
    let tray_style = settings.tray_icon.clone();
    config::save_config(settings).await?;
    update_tray_icon(&app, &tray_style);
    Ok(())
}

fn update_tray_icon(app: &tauri::AppHandle, style: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let png_data: &[u8] = if style == "mono" {
            include_bytes!("../icons/tray_mono.png")
        } else {
            include_bytes!("../icons/tray.png")
        };
        if let Ok(icon) = tauri::image::Image::from_bytes(png_data) {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_icon_as_template(style == "mono");
        }
    }
}

// ── Auto-sync ──

static AUTO_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

fn start_auto_sync(app: tauri::AppHandle) {
    if AUTO_SYNC_RUNNING.swap(true, Ordering::SeqCst) { return; }

    tauri::async_runtime::spawn(async move {
        let mut last_sync_date = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;

            let cfg = config::get_config().await;
            if !cfg.auto_sync_enabled { continue; }

            let now = chrono::Local::now();
            let today = now.format("%Y-%m-%d").to_string();
            let current_time = now.format("%H:%M").to_string();

            // Already synced today?
            if last_sync_date == today { continue; }

            // Is it past the configured time?
            if current_time >= cfg.auto_sync_time {
                let target_name = match current_target(&cfg) {
                    Target::YouTrack => "YouTrack",
                    Target::Jira => "Jira",
                };
                if let Ok(result) = sync(today.clone(), today.clone(), None).await {
                    last_sync_date = today;
                    notify_sync_result(&app, &result, "Auto-synced", target_name);
                }
            }
        }
    });
}

/// Native macOS notification for a background sync. Silent when nothing changed.
fn notify_sync_result(app: &tauri::AppHandle, result: &SyncResponse, verb: &str, target_name: &str) {
    let mut lines = Vec::new();
    if result.synced > 0 {
        lines.push(format!("{} {} work items to {}", verb, result.synced, target_name));
    }
    if result.deletes_pending > 0 {
        lines.push(format!("{} deletions need confirmation — open Synclock and sync", result.deletes_pending));
    }
    if result.failed > 0 {
        lines.push(format!("{} failed", result.failed));
    }
    if lines.is_empty() { return; }
    let _ = tauri_plugin_notification::NotificationExt::notification(app)
        .builder()
        .title("Synclock")
        .body(lines.join("\n"))
        .show();
}

// ── Window management ──

fn show_window(app: &tauri::AppHandle, position: tauri::PhysicalPosition<f64>) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            return;
        }

        // Get scale factor for Retina displays
        let scale = window.scale_factor().unwrap_or(2.0);
        let window_width_physical = 380.0 * scale;

        // Center window horizontally under the tray icon click position
        let x = (position.x - window_width_physical / 2.0).max(0.0) as i32;
        // Place right below the macOS menu bar (menu bar is ~25 logical px = ~50 physical px)
        let y = (25.0 * scale) as i32;

        let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)));
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("panel-opened", ());
    }
}

// ── App entry ──

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    config::load_config();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            // Hide on focus loss
            let handle_blur = app.handle().clone();
            if let Some(window) = app.get_webview_window("main") {
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        if let Some(w) = handle_blur.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                });
            }

            // Tray icon click
            let handle_tray = app.handle().clone();
            let tray = app.tray_by_id("main").expect("tray not found");
            let menu = MenuBuilder::new(app)
                .item(&MenuItemBuilder::with_id("sync_today", "Sync Today").build(app)?)
                .separator()
                .item(&MenuItemBuilder::with_id("settings", "Settings...").build(app)?)
                .separator()
                .item(&MenuItemBuilder::with_id("quit", "Quit").build(app)?)
                .build()?;
            tray.set_menu(Some(menu))?;
            tray.set_show_menu_on_left_click(false)?;

            tray.on_tray_icon_event(move |_tray, event| {
                if let TrayIconEvent::Click { position, button, button_state, .. } = event {
                    if matches!(button, tauri::tray::MouseButton::Left)
                        && matches!(button_state, tauri::tray::MouseButtonState::Up) {
                        show_window(&handle_tray, position);
                    }
                }
            });

            app.on_menu_event(move |app, event| {
                match event.id().as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    "settings" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit("show-settings", ());
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "sync_today" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                            let result = sync(today.clone(), today, None).await;
                            if let Ok(r) = &result {
                                let target_name = match current_target(&config::get_config().await) {
                                    Target::YouTrack => "YouTrack",
                                    Target::Jira => "Jira",
                                };
                                notify_sync_result(&app, r, "Synced", target_name);
                            }
                            let _ = app.emit("quick-sync-result", &result);
                        });
                    }
                    _ => {}
                }
            });

            // Set tray icon from config
            {
                let cfg = tauri::async_runtime::block_on(config::get_config());
                update_tray_icon(app.handle(), &cfg.tray_icon);
            }

            // Start auto-sync
            start_auto_sync(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![check_status, preview, sync, get_settings, save_settings, get_early_activities, get_youtrack_work_item_types])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
