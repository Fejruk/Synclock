//! Pure reconciliation between time-tracker entries and YouTrack work items.
//!
//! Given what each entry *should* look like in YouTrack and the work items that
//! already carry a synclock marker, `plan` decides which items to create, update
//! or delete. No HTTP here — the caller fetches both sides and executes the ops.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

const DAY_MS: i64 = 86_400_000;

/// One work item an entry should produce (one per issue key).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Desired {
    /// Resolved YouTrack readable id (e.g. `PROJ-12`).
    pub issue: String,
    pub minutes: i64,
    /// UTC midnight of the entry's local start day, in ms.
    pub date_ms: i64,
    /// `None` = no mapping configured → leave the item's type alone.
    pub type_id: Option<String>,
    /// Work item text without the marker.
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct EntryInput {
    pub entry_id: String,
    /// Inside the reconciled window. Entries outside it (fetched as a buffer)
    /// only follow up on items that already exist — they never create new ones.
    pub in_window: bool,
    /// `Err` when an issue key could not be resolved: the entry is left alone
    /// entirely, including its existing items.
    pub targets: Result<Vec<Desired>, String>,
}

/// An existing work item of the current user that carries a marker of the
/// current provider.
#[derive(Debug, Clone)]
pub struct Existing {
    pub id: String,
    pub entry_id: String,
    pub issue: String,
    pub minutes: i64,
    pub date_ms: i64,
    pub type_id: Option<String>,
    /// Full text including the marker.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "field", rename_all = "snake_case")]
pub enum Change {
    Duration { from: i64, to: i64 },
    Date { from: i64, to: i64 },
    Type { from: Option<String>, to: String },
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteReason {
    /// Entry no longer exists in the time tracker.
    Orphan,
    /// Entry exists but no longer targets this issue (key changed or removed).
    NotTarget,
    /// Another item with the same marker on the same issue is kept.
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Op {
    Keep { entry_id: String, item_id: String, issue: String, minutes: i64 },
    Create { entry_id: String, desired: Desired },
    Update { entry_id: String, item_id: String, desired: Desired, changes: Vec<Change> },
    Delete { entry_id: String, item_id: String, issue: String, minutes: i64, date_ms: i64, reason: DeleteReason },
    /// Entry could not be planned (e.g. unknown issue); its items are untouched.
    Blocked { entry_id: String, error: String },
}

impl Op {
    pub fn entry_id(&self) -> &str {
        match self {
            Op::Keep { entry_id, .. }
            | Op::Create { entry_id, .. }
            | Op::Update { entry_id, .. }
            | Op::Delete { entry_id, .. }
            | Op::Blocked { entry_id, .. } => entry_id,
        }
    }
}

pub fn marker(provider_marker: &str) -> String {
    format!("[synclock:{}]", provider_marker)
}

/// Extract the entry id from a `[synclock:<provider>-<id>]` marker. Only markers
/// of `provider` count, so switching providers never touches the other's items.
pub fn parse_entry_id(text: &str, provider: &str) -> Option<String> {
    let prefix = format!("[synclock:{}-", provider);
    let start = text.rfind(&prefix)? + prefix.len();
    let len = text[start..].find(']')?;
    let id = &text[start..start + len];
    (!id.is_empty()).then(|| id.to_string())
}

/// Work item text as stored: the comment followed by the marker.
pub fn compose_text(text: &str, provider_marker: &str) -> String {
    if text.is_empty() {
        marker(provider_marker)
    } else {
        format!("{}\n\n{}", text, marker(provider_marker))
    }
}

/// Text without any synclock marker, with whitespace normalized so formatting
/// differences introduced by YouTrack don't cause endless updates.
fn normalize_text(text: &str) -> String {
    let mut out = text.replace("\r\n", "\n");
    while let Some(start) = out.find("[synclock:") {
        match out[start..].find(']') {
            Some(len) => out.replace_range(start..start + len + 1, ""),
            None => break,
        }
    }
    out.trim().to_string()
}

fn day_of(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

fn diff(item: &Existing, want: &Desired) -> Vec<Change> {
    let mut changes = Vec::new();
    if item.minutes != want.minutes {
        changes.push(Change::Duration { from: item.minutes, to: want.minutes });
    }
    if day_of(item.date_ms) != day_of(want.date_ms) {
        changes.push(Change::Date { from: item.date_ms, to: want.date_ms });
    }
    if let Some(t) = &want.type_id {
        if item.type_id.as_deref() != Some(t.as_str()) {
            changes.push(Change::Type { from: item.type_id.clone(), to: t.clone() });
        }
    }
    if normalize_text(&item.text) != normalize_text(&want.text) {
        changes.push(Change::Text);
    }
    changes
}

/// Merge targets that resolve to the same issue (e.g. a legacy alias and the
/// real key both mentioned) into one, summing minutes.
fn merge_targets(targets: &[Desired]) -> Vec<Desired> {
    let mut out: Vec<Desired> = Vec::new();
    for t in targets {
        match out.iter_mut().find(|o| o.issue == t.issue) {
            Some(o) => o.minutes += t.minutes,
            None => out.push(t.clone()),
        }
    }
    out
}

/// Decide the operations that bring YouTrack in line with the entries.
///
/// `window` is the inclusive `(from, to)` range of work item dates (UTC-midnight
/// ms) that was reconciled; orphans are only deleted inside it.
pub fn plan(entries: &[EntryInput], existing: &[Existing], window: (i64, i64)) -> Vec<Op> {
    let mut by_entry: HashMap<&str, Vec<&Existing>> = HashMap::new();
    for item in existing {
        by_entry.entry(item.entry_id.as_str()).or_default().push(item);
    }

    let mut ops = Vec::new();
    let mut known: HashSet<&str> = HashSet::new();

    for entry in entries {
        known.insert(entry.entry_id.as_str());
        let items = by_entry.get(entry.entry_id.as_str()).cloned().unwrap_or_default();
        if !entry.in_window && items.is_empty() {
            continue;
        }

        let targets = match &entry.targets {
            Ok(t) => merge_targets(t),
            Err(e) => {
                ops.push(Op::Blocked { entry_id: entry.entry_id.clone(), error: e.clone() });
                continue;
            }
        };

        let mut used: HashSet<&str> = HashSet::new();
        for want in &targets {
            let candidates: Vec<&Existing> = items.iter().copied().filter(|i| i.issue == want.issue).collect();
            // Keep the candidate needing the fewest changes; the rest are duplicates.
            let best = candidates.iter().copied().min_by_key(|i| diff(i, want).len());
            match best {
                None => ops.push(Op::Create { entry_id: entry.entry_id.clone(), desired: want.clone() }),
                Some(item) => {
                    used.insert(item.id.as_str());
                    let changes = diff(item, want);
                    if changes.is_empty() {
                        ops.push(Op::Keep {
                            entry_id: entry.entry_id.clone(),
                            item_id: item.id.clone(),
                            issue: item.issue.clone(),
                            minutes: item.minutes,
                        });
                    } else {
                        ops.push(Op::Update {
                            entry_id: entry.entry_id.clone(),
                            item_id: item.id.clone(),
                            desired: want.clone(),
                            changes,
                        });
                    }
                    for dup in candidates.iter().filter(|c| c.id != item.id) {
                        used.insert(dup.id.as_str());
                        ops.push(delete(dup, DeleteReason::Duplicate));
                    }
                }
            }
        }

        for item in items.iter().filter(|i| !used.contains(i.id.as_str())) {
            ops.push(delete(item, DeleteReason::NotTarget));
        }
    }

    let (from, to) = (day_of(window.0), day_of(window.1));
    for item in existing {
        let day = day_of(item.date_ms);
        if !known.contains(item.entry_id.as_str()) && day >= from && day <= to {
            ops.push(delete(item, DeleteReason::Orphan));
        }
    }

    ops
}

/// Deletions run unattended only when they look routine: at most `max` of
/// them, and never when the tracker returned no entries at all for the window
/// (more likely an API hiccup than a deliberately emptied fortnight).
pub fn deletes_need_confirmation(deletes: usize, entries_in_window: usize, max: usize) -> bool {
    deletes > 0 && (deletes > max || entries_in_window == 0)
}

fn delete(item: &Existing, reason: DeleteReason) -> Op {
    Op::Delete {
        entry_id: item.entry_id.clone(),
        item_id: item.id.clone(),
        issue: item.issue.clone(),
        minutes: item.minutes,
        date_ms: item.date_ms,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D1: i64 = 20_000 * DAY_MS;
    const D2: i64 = 20_001 * DAY_MS;
    const WINDOW: (i64, i64) = (D1, D2);

    fn want(issue: &str, minutes: i64) -> Desired {
        Desired {
            issue: issue.into(),
            minutes,
            date_ms: D1,
            type_id: Some("dev".into()),
            text: "Coding - fix bug".into(),
        }
    }

    fn entry(id: &str, targets: Vec<Desired>) -> EntryInput {
        EntryInput { entry_id: id.into(), in_window: true, targets: Ok(targets) }
    }

    fn item(id: &str, entry_id: &str, issue: &str, minutes: i64) -> Existing {
        Existing {
            id: id.into(),
            entry_id: entry_id.into(),
            issue: issue.into(),
            minutes,
            date_ms: D1,
            type_id: Some("dev".into()),
            text: compose_text("Coding - fix bug", &format!("early-{}", entry_id)),
        }
    }

    fn kinds(ops: &[Op]) -> Vec<String> {
        let mut v: Vec<String> = ops
            .iter()
            .map(|op| match op {
                Op::Keep { item_id, .. } => format!("keep {}", item_id),
                Op::Create { desired, .. } => format!("create {} {}", desired.issue, desired.minutes),
                Op::Update { item_id, .. } => format!("update {}", item_id),
                Op::Delete { item_id, reason, .. } => format!("delete {} {:?}", item_id, reason),
                Op::Blocked { entry_id, .. } => format!("blocked {}", entry_id),
            })
            .collect();
        v.sort();
        v
    }

    fn changes_of(ops: &[Op]) -> Vec<Change> {
        ops.iter()
            .find_map(|op| match op {
                Op::Update { changes, .. } => Some(changes.clone()),
                _ => None,
            })
            .expect("no update op")
    }

    #[test]
    fn unchanged_entry_is_kept() {
        let ops = plan(&[entry("e1", vec![want("P-1", 30)])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["keep w1"]);
    }

    #[test]
    fn missing_item_is_created() {
        let ops = plan(&[entry("e1", vec![want("P-1", 30)])], &[], WINDOW);
        assert_eq!(kinds(&ops), vec!["create P-1 30"]);
    }

    #[test]
    fn duration_change_updates() {
        let ops = plan(&[entry("e1", vec![want("P-1", 45)])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["update w1"]);
        assert_eq!(changes_of(&ops), vec![Change::Duration { from: 30, to: 45 }]);
    }

    #[test]
    fn ticket_change_deletes_old_and_creates_new() {
        let ops = plan(&[entry("e1", vec![want("P-2", 30)])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["create P-2 30", "delete w1 NotTarget"]);
    }

    #[test]
    fn type_change_updates() {
        let mut w = want("P-1", 30);
        w.type_id = Some("meeting".into());
        let ops = plan(&[entry("e1", vec![w])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(
            changes_of(&ops),
            vec![Change::Type { from: Some("dev".into()), to: "meeting".into() }]
        );
    }

    #[test]
    fn unmapped_type_is_left_alone() {
        let mut w = want("P-1", 30);
        w.type_id = None;
        let ops = plan(&[entry("e1", vec![w])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["keep w1"]);
    }

    #[test]
    fn day_change_updates() {
        let mut w = want("P-1", 30);
        w.date_ms = D2;
        let ops = plan(&[entry("e1", vec![w])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(changes_of(&ops), vec![Change::Date { from: D1, to: D2 }]);
    }

    #[test]
    fn same_day_different_time_of_day_is_not_a_change() {
        let mut existing = item("w1", "e1", "P-1", 30);
        existing.date_ms = D1 + 3_600_000;
        let ops = plan(&[entry("e1", vec![want("P-1", 30)])], &[existing], WINDOW);
        assert_eq!(kinds(&ops), vec!["keep w1"]);
    }

    #[test]
    fn text_change_updates_but_whitespace_does_not() {
        let mut existing = item("w1", "e1", "P-1", 30);
        existing.text = "  Coding - fix bug\r\n\r\n[synclock:early-e1]\n".into();
        let ops = plan(&[entry("e1", vec![want("P-1", 30)])], &[existing.clone()], WINDOW);
        assert_eq!(kinds(&ops), vec!["keep w1"]);

        let mut w = want("P-1", 30);
        w.text = "Coding - fix another bug".into();
        let ops = plan(&[entry("e1", vec![w])], &[existing], WINDOW);
        assert_eq!(changes_of(&ops), vec![Change::Text]);
    }

    #[test]
    fn deleted_entry_orphans_are_deleted() {
        let ops = plan(&[], &[item("w1", "gone", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["delete w1 Orphan"]);
    }

    #[test]
    fn orphans_outside_window_are_left_alone() {
        let mut old = item("w1", "gone", "P-1", 30);
        old.date_ms = D1 - DAY_MS;
        let ops = plan(&[], &[old], WINDOW);
        assert!(ops.is_empty());
    }

    #[test]
    fn duplicate_markers_keep_one() {
        let mut stale = item("w2", "e1", "P-1", 10);
        stale.text = compose_text("old", "early-e1");
        let ops = plan(
            &[entry("e1", vec![want("P-1", 30)])],
            &[stale, item("w1", "e1", "P-1", 30)],
            WINDOW,
        );
        assert_eq!(kinds(&ops), vec!["delete w2 Duplicate", "keep w1"]);
    }

    #[test]
    fn multi_key_entry_splits_minutes() {
        let ops = plan(&[entry("e1", vec![want("P-1", 30), want("P-2", 30)])], &[], WINDOW);
        assert_eq!(kinds(&ops), vec!["create P-1 30", "create P-2 30"]);
    }

    #[test]
    fn removing_one_key_deletes_it_and_resizes_the_other() {
        let ops = plan(
            &[entry("e1", vec![want("P-1", 60)])],
            &[item("w1", "e1", "P-1", 30), item("w2", "e1", "P-2", 30)],
            WINDOW,
        );
        assert_eq!(kinds(&ops), vec!["delete w2 NotTarget", "update w1"]);
    }

    #[test]
    fn keys_resolving_to_same_issue_are_merged() {
        let ops = plan(&[entry("e1", vec![want("P-1", 30), want("P-1", 30)])], &[], WINDOW);
        assert_eq!(kinds(&ops), vec!["create P-1 60"]);
    }

    #[test]
    fn entry_without_targets_deletes_its_items() {
        let ops = plan(&[entry("e1", vec![])], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["delete w1 NotTarget"]);
    }

    #[test]
    fn unresolved_entry_blocks_and_keeps_items() {
        let e = EntryInput { entry_id: "e1".into(), in_window: true, targets: Err("not found".into()) };
        let ops = plan(&[e], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["blocked e1"]);
    }

    #[test]
    fn buffer_entry_without_items_is_not_created() {
        let mut e = entry("e1", vec![want("P-1", 30)]);
        e.in_window = false;
        assert!(plan(&[e], &[], WINDOW).is_empty());
    }

    #[test]
    fn entry_moved_out_of_window_follows_its_item() {
        // Item sits in the window, entry was moved to a day in the buffer.
        let mut w = want("P-1", 30);
        w.date_ms = D1 - 5 * DAY_MS;
        let mut e = entry("e1", vec![w]);
        e.in_window = false;
        let ops = plan(&[e], &[item("w1", "e1", "P-1", 30)], WINDOW);
        assert_eq!(kinds(&ops), vec!["update w1"]);
    }

    #[test]
    fn entry_moved_into_window_updates_item_outside_it() {
        let mut old = item("w1", "e1", "P-1", 30);
        old.date_ms = D1 - 5 * DAY_MS;
        let ops = plan(&[entry("e1", vec![want("P-1", 30)])], &[old], WINDOW);
        assert_eq!(kinds(&ops), vec!["update w1"]);
    }

    #[test]
    fn delete_confirmation_policy() {
        assert!(!deletes_need_confirmation(0, 0, 10));
        assert!(!deletes_need_confirmation(10, 5, 10));
        assert!(deletes_need_confirmation(11, 5, 10));
        assert!(deletes_need_confirmation(1, 0, 10));
    }

    #[test]
    fn manual_work_items_never_become_candidates() {
        // Items without a marker (typed in YouTrack by hand) are dropped before
        // planning because no entry id can be parsed from them.
        assert_eq!(parse_entry_id("Code review for P-1", "early"), None);
        assert_eq!(parse_entry_id("see [synclock] notes", "early"), None);
    }

    #[test]
    fn parses_marker_of_current_provider_only() {
        assert_eq!(parse_entry_id("x\n\n[synclock:early-abc123]", "early"), Some("abc123".into()));
        assert_eq!(parse_entry_id("[synclock:toggl-42]", "early"), None);
        assert_eq!(parse_entry_id("manual work item", "early"), None);
        assert_eq!(parse_entry_id("[synclock:early-]", "early"), None);
        assert_eq!(parse_entry_id("[synclock:early-abc", "early"), None);
    }
}
