mod config;
mod early;
mod jira;
mod toggl;
mod youtrack;

use serde::Serialize;
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
}

#[derive(Clone, Serialize)]
struct PreviewResponse {
    total: usize,
    with_jira: usize,
    items: Vec<PreviewItem>,
}

#[derive(Clone, Serialize)]
struct SyncResultItem {
    entry_id: String,
    activity: String,
    issue_key: String,
    duration: String,
    success: bool,
    skipped: bool,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct SyncResponse {
    synced: usize,
    skipped: usize,
    failed: usize,
    results: Vec<SyncResultItem>,
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
    match cfg.provider.as_str() {
        "toggl" => fetch_toggl_entries(from, to).await,
        _ => fetch_early_entries(from, to).await,
    }
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
    let target = current_target(&cfg);
    let entries = fetch_entries(&from, &to).await?;

    let mut all_keys = std::collections::HashSet::new();
    for e in &entries {
        for k in &e.jira_keys { all_keys.insert(k.clone()); }
    }

    let mut jira_map: std::collections::HashMap<String, Vec<jira::Worklog>> = std::collections::HashMap::new();
    let mut yt_map: std::collections::HashMap<String, Vec<youtrack::WorkItem>> = std::collections::HashMap::new();

    match target {
        Target::Jira => {
            for key in &all_keys {
                let wls = jira::get_worklogs(key).await.unwrap_or_default();
                jira_map.insert(key.clone(), wls);
            }
        }
        Target::YouTrack => {
            for key in &all_keys {
                let items = youtrack::get_work_items(key).await.unwrap_or_default();
                yt_map.insert(key.clone(), items);
            }
        }
    }

    let items: Vec<PreviewItem> = entries.iter().map(|e| {
        let per_key = if !e.jira_keys.is_empty() {
            (e.duration_min as f64 / e.jira_keys.len() as f64).round().max(1.0) as i64
        } else { 0 };

        let synced = !e.jira_keys.is_empty() && match target {
            Target::Jira => e.jira_keys.iter().all(|k| {
                let wls = jira_map.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
                jira::is_already_synced(wls, &e.started_at, per_key)
            }),
            Target::YouTrack => {
                let marker = dedup_marker_for(&cfg.provider, &e.id);
                e.jira_keys.iter().all(|k| {
                    let items = yt_map.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
                    youtrack::is_already_synced(items, &marker)
                })
            }
        };

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
            synced,
        }
    }).collect();

    let with_jira = items.iter().filter(|i| i.has_jira_key).count();
    Ok(PreviewResponse { total: items.len(), with_jira, items })
}

#[tauri::command]
async fn sync(from: String, to: String) -> Result<SyncResponse, String> {
    let cfg = config::get_config().await;
    let target = current_target(&cfg);
    let entries = fetch_entries(&from, &to).await?;
    let mut results = Vec::new();

    for e in &entries {
        if e.jira_keys.is_empty() || e.duration_min < 1 { continue; }

        let per_key = (e.duration_min as f64 / e.jira_keys.len() as f64).round().max(1.0) as i64;
        let comment = if e.note.is_empty() { e.activity.clone() } else { format!("{} - {}", e.activity, e.note) };
        let marker = dedup_marker_for(&cfg.provider, &e.id);

        for key in &e.jira_keys {
            let already_synced = match target {
                Target::Jira => {
                    let wls = jira::get_worklogs(key).await.unwrap_or_default();
                    jira::is_already_synced(&wls, &e.started_at, per_key)
                }
                Target::YouTrack => {
                    let items = youtrack::get_work_items(key).await.unwrap_or_default();
                    youtrack::is_already_synced(&items, &marker)
                }
            };

            if already_synced {
                results.push(SyncResultItem {
                    entry_id: e.id.clone(), activity: e.activity.clone(),
                    issue_key: key.clone(), duration: String::new(),
                    success: true, skipped: true, error: None,
                });
                continue;
            }

            let create_result = match target {
                Target::Jira => jira::add_worklog(key, per_key, &e.started_at, &comment).await.map(|_| ()),
                Target::YouTrack => {
                    let type_id = e.activity_id.as_ref()
                        .and_then(|aid| cfg.activity_type_map.get(aid))
                        .filter(|s| !s.is_empty())
                        .map(|s| s.as_str());
                    youtrack::add_work_item(key, per_key, &e.started_at, &comment, &marker, type_id).await.map(|_| ())
                }
            };

            match create_result {
                Ok(_) => results.push(SyncResultItem {
                    entry_id: e.id.clone(), activity: e.activity.clone(),
                    issue_key: key.clone(), duration: fmt_duration(per_key),
                    success: true, skipped: false, error: None,
                }),
                Err(err) => results.push(SyncResultItem {
                    entry_id: e.id.clone(), activity: e.activity.clone(),
                    issue_key: key.clone(), duration: String::new(),
                    success: false, skipped: false, error: Some(err),
                }),
            }
        }
    }

    let synced = results.iter().filter(|r| r.success && !r.skipped).count();
    let skipped = results.iter().filter(|r| r.skipped).count();
    let failed = results.iter().filter(|r| !r.success).count();
    Ok(SyncResponse { synced, skipped, failed, results })
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
                if let Ok(result) = sync(today.clone(), today.clone()).await {
                    last_sync_date = today;
                    if result.synced > 0 {
                        // Native macOS notification
                        let _ = tauri_plugin_notification::NotificationExt::notification(&app)
                            .builder()
                            .title("Synclock")
                            .body(format!("Auto-synced {} entries to Jira", result.synced))
                            .show();
                    }
                }
            }
        }
    });
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
                            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                            let result = sync(today.clone(), today).await;
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
