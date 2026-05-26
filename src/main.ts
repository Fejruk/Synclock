import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const $ = (id: string) => document.getElementById(id)!;
const esc = (s: string | null | undefined) => {
  const d = document.createElement("div");
  d.textContent = s ?? "";
  return d.innerHTML;
};
const fmtDur = (min: number) => {
  const h = Math.floor(min / 60), m = min % 60;
  return h > 0 && m > 0 ? `${h}h ${m}m` : h > 0 ? `${h}h` : `${m}m`;
};
const fmtTime = (iso: string) => {
  if (!iso) return "–";
  // Handle all formats: UTC without Z (Early), ISO with Z, ISO with +offset (Toggl)
  let str = iso;
  if (!str.endsWith("Z") && !str.includes("+") && !str.match(/\d{2}:\d{2}$/)) {
    str += "Z";
  } else if (!str.endsWith("Z") && !str.includes("+") && !str.includes("-", 10)) {
    str += "Z";
  }
  const d = new Date(str);
  if (isNaN(d.getTime())) return "–";
  return d.toLocaleTimeString("cs-CZ", { hour: "2-digit", minute: "2-digit" });
};

// ── Types ──

interface PreviewItem {
  id: string; activity: string; activity_color: string;
  jira_keys: string[]; duration_min: number;
  started_at: string; stopped_at: string;
  note: string; has_jira_key: boolean; synced: boolean;
}
interface PreviewResponse { total: number; with_jira: number; items: PreviewItem[]; }
interface SyncResultItem {
  entry_id: string; activity: string; issue_key: string;
  duration: string; success: boolean; skipped: boolean; error: string | null;
}
interface SyncResponse { synced: number; skipped: number; failed: number; results: SyncResultItem[]; }
interface Settings {
  provider: string;
  early_api_key: string; early_api_secret: string;
  toggl_api_token: string;
  target: string;
  jira_base_url: string; jira_email: string; jira_api_token: string;
  youtrack_base_url: string; youtrack_token: string;
  default_issue_key: string;
  activity_type_map: Record<string, string>;
  auto_sync_enabled: boolean;
  auto_sync_time: string;
  tray_icon: string;
}

interface ActivityOption { id: string; name: string; color: string; }
interface YoutrackType { id: string; name: string; }

let cachedActivities: ActivityOption[] = [];
let cachedYtTypes: YoutrackType[] = [];
let currentMapping: Record<string, string> = {};

const targetLabel = (t: string) => t === "youtrack" ? "YouTrack" : "Jira";

// ── Date state ──

let currentDate = new Date();
let calendarOpen = false;

function dateStr(d: Date) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function formatDateLabel(d: Date): string {
  return d.toLocaleDateString("cs-CZ", { weekday: "short", day: "numeric", month: "short" });
}

function updateDateLabel() {
  $("dateLabelText").textContent = formatDateLabel(currentDate);
}

function shiftDay(delta: number) {
  currentDate.setDate(currentDate.getDate() + delta);
  updateDateLabel();
  closeCalendar();
  doPreview();
}

// ── Calendar ──

function openCalendar() {
  calendarOpen = true;
  $("calendar").style.display = "block";
  renderCalendar();
}

function closeCalendar() {
  calendarOpen = false;
  $("calendar").style.display = "none";
}

function toggleCalendar() {
  if (calendarOpen) closeCalendar(); else openCalendar();
}

let calViewYear = 0;
let calViewMonth = 0;

function renderCalendar() {
  calViewYear = calViewYear || currentDate.getFullYear();
  calViewMonth = calViewMonth || currentDate.getMonth();

  const year = calViewYear;
  const month = calViewMonth;
  const today = new Date();
  const selected = dateStr(currentDate);

  const monthNames = ["Leden", "Únor", "Březen", "Duben", "Květen", "Červen",
    "Červenec", "Srpen", "Září", "Říjen", "Listopad", "Prosinec"];
  const dayNames = ["Po", "Út", "St", "Čt", "Pá", "So", "Ne"];

  const firstDay = new Date(year, month, 1);
  let startDow = firstDay.getDay() - 1;
  if (startDow < 0) startDow = 6;
  const daysInMonth = new Date(year, month + 1, 0).getDate();

  let html = `
    <div class="cal-hdr">
      <button class="cal-nav" id="calPrev">&lsaquo;</button>
      <span class="cal-title">${monthNames[month]} ${year}</span>
      <button class="cal-nav" id="calNext">&rsaquo;</button>
    </div>
    <div class="cal-days">
      ${dayNames.map(d => `<span class="cal-dow">${d}</span>`).join("")}
  `;

  // Empty cells before first day
  for (let i = 0; i < startDow; i++) {
    html += `<span class="cal-day cal-empty"></span>`;
  }

  for (let day = 1; day <= daysInMonth; day++) {
    const d = new Date(year, month, day);
    const ds = dateStr(d);
    const isToday = ds === dateStr(today);
    const isSel = ds === selected;
    const cls = ["cal-day"];
    if (isToday) cls.push("cal-today");
    if (isSel) cls.push("cal-sel");
    html += `<span class="${cls.join(" ")}" data-date="${ds}">${day}</span>`;
  }

  html += `</div>`;
  $("calendar").innerHTML = html;

  // Events
  $("calPrev").addEventListener("click", (e) => {
    e.stopPropagation();
    calViewMonth--;
    if (calViewMonth < 0) { calViewMonth = 11; calViewYear--; }
    renderCalendar();
  });
  $("calNext").addEventListener("click", (e) => {
    e.stopPropagation();
    calViewMonth++;
    if (calViewMonth > 11) { calViewMonth = 0; calViewYear++; }
    renderCalendar();
  });

  $("calendar").querySelectorAll(".cal-day[data-date]").forEach((el) => {
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      const val = (el as HTMLElement).dataset.date!;
      currentDate = new Date(val + "T12:00:00");
      updateDateLabel();
      closeCalendar();
      doPreview();
    });
  });
}

// ── Views ──

function showView(id: string) {
  document.querySelectorAll(".view").forEach((v) => v.classList.remove("active"));
  $(id).classList.add("active");
}

// ── Status ──

async function checkStatus() {
  const provDot = $("provDot"), targetDot = $("targetDot");
  const provStatus = $("provStatus"), targetStatus = $("targetStatus");
  const provLabel = $("provLabel"), tgtLabel = $("targetLabel");

  try {
    const data = await invoke<any>("check_status");
    const provider = data.provider === "toggl" ? "Toggl" : "Early";
    const target = targetLabel(data.target);
    provLabel.textContent = provider;
    tgtLabel.textContent = target;
    $("title").innerHTML = `${provider} <em>&rarr;</em> ${target}`;
    ($("btnSync") as HTMLButtonElement).textContent = `Sync to ${target}`;

    provDot.className = "conn-dot " + (data.provider_ok ? "ok" : "err");
    provStatus.textContent = data.provider_ok ? "OK" : "Error";

    targetDot.className = "conn-dot " + (data.target_check?.ok ? "ok" : "err");
    targetStatus.textContent = data.target_check?.ok ? "OK" : "Error";
  } catch (e) {
    provDot.className = "conn-dot err";
    targetDot.className = "conn-dot err";
    provStatus.textContent = "Error";
    targetStatus.textContent = "Error";
  }
}

// ── Preview ──

function renderPreview(data: PreviewResponse) {
  $("previewSection").style.display = "block";
  $("logSection").style.display = "none";

  const totalMin = data.items.reduce((s, i) => s + i.duration_min, 0);
  const toSync = data.items.filter((i) => i.has_jira_key && !i.synced);
  const syncMin = toSync.reduce((s, i) => s + i.duration_min, 0);
  const synced = data.items.filter((i) => i.synced).length;

  $("summary").innerHTML = [
    `<div class="st"><b>${data.total}</b> entries</div>`,
    toSync.length ? `<div class="st"><b>${toSync.length}</b> to sync</div>` : '',
    synced ? `<div class="st"><b>${synced}</b> synced</div>` : '',
    `<div class="st"><b>${fmtDur(totalMin)}</b> total</div>`,
    syncMin ? `<div class="st"><b>${fmtDur(syncMin)}</b> pending</div>` : '',
  ].filter(Boolean).join('');

  if (data.items.length === 0) {
    $("entries").innerHTML = '<div class="empty">No entries for this day</div>';
  } else {
    $("entries").innerHTML = data.items.map((item) => `
      <div class="ent" style="opacity:${item.has_jira_key && !item.synced ? 1 : 0.4}">
        <div class="ent-d" style="background:#${esc(item.activity_color)}"></div>
        <div class="ent-b">
          <div class="ent-t">${esc(item.activity)}${item.synced ? '<span class="ent-sd">synced</span>' : ""}</div>
          <div class="ent-s">${fmtTime(item.started_at)} – ${fmtTime(item.stopped_at)}${item.note ? " · " + esc(item.note) : ""}</div>
        </div>
        <div class="ent-r">
          <div class="ent-dur">${fmtDur(item.duration_min)}</div>
          ${item.jira_keys.map((k) => `<span class="tag tag-j">${esc(k)}</span>`).join(" ")}
          ${!item.has_jira_key ? '<span class="tag tag-n">–</span>' : ""}
        </div>
      </div>
    `).join("");
  }

  ($("btnSync") as HTMLButtonElement).disabled = toSync.length === 0;
  $("syncHint").textContent = toSync.length === 0 && data.items.length > 0 ? "All synced" : "";
}

async function doPreview() {
  const day = dateStr(currentDate);
  const refreshBtn = $("btnRefresh");
  refreshBtn.classList.add("spinning");
  $("previewSection").style.display = "block";
  $("entries").innerHTML = '<div class="empty">Loading...</div>';
  $("summary").innerHTML = '';
  try {
    renderPreview(await invoke<PreviewResponse>("preview", { from: day, to: day }));
  } catch (e) {
    $("entries").innerHTML = `<div class="empty">${esc(String(e))}</div>`;
  } finally {
    refreshBtn.classList.remove("spinning");
  }
}

async function doSync() {
  const day = dateStr(currentDate);
  const btn = $("btnSync") as HTMLButtonElement;
  btn.disabled = true; btn.textContent = "Syncing...";
  $("logSection").style.display = "block";
  const log = $("log");
  log.innerHTML = '<div class="l-dm">Starting sync...</div>';

  try {
    const data = await invoke<SyncResponse>("sync", { from: day, to: day });
    let html = "";
    for (const r of data.results) {
      if (r.skipped) html += `<div class="l-dm">– ${esc(r.issue_key)} synced</div>`;
      else if (r.success) html += `<div class="l-ok">✓ ${esc(r.issue_key)} ← ${r.duration}</div>`;
      else html += `<div class="l-er">✗ ${esc(r.issue_key)} ${esc(r.error)}</div>`;
    }
    html += `<div class="l-dm" style="margin-top:6px">${data.synced} synced · ${data.skipped} skipped · ${data.failed} failed</div>`;
    log.innerHTML = html;
    setTimeout(() => doPreview(), 500);
  } catch (e) { log.innerHTML = `<div class="l-er">${esc(String(e))}</div>`; }
  finally {
    btn.disabled = false;
    // Restore label using current settings target.
    try {
      const s = await invoke<Settings>("get_settings");
      btn.textContent = `Sync to ${targetLabel(s.target)}`;
    } catch { btn.textContent = "Sync"; }
  }
}

// ── Settings ──

function toggleProviderFields(provider: string) {
  $("earlyFields").style.display = provider === "early" ? "block" : "none";
  $("togglFields").style.display = provider === "toggl" ? "block" : "none";
}

function toggleTargetFields(target: string) {
  $("jiraFields").style.display = target === "jira" ? "block" : "none";
  $("youtrackFields").style.display = target === "youtrack" ? "block" : "none";
  updateActivityMapVisibility();
}

function updateActivityMapVisibility() {
  const provider = ($("setProvider") as HTMLSelectElement).value;
  const target = ($("setTarget") as HTMLSelectElement).value;
  const show = provider === "early" && target === "youtrack";
  $("activityMapFields").style.display = show ? "block" : "none";
  if (show) loadActivityMap();
}

function renderActivityMap() {
  const rows = $("activityMapRows");
  const status = $("activityMapStatus");

  if (cachedActivities.length === 0) {
    status.textContent = "No Early activities found — fill in API key/secret and save first.";
    rows.innerHTML = "";
    return;
  }

  status.style.display = "none";

  rows.innerHTML = cachedActivities.map((a) => {
    const selected = currentMapping[a.id] ?? "";
    const options = [
      `<option value="">(no type)</option>`,
      ...cachedYtTypes.map((t) => `<option value="${esc(t.id)}" ${t.id === selected ? "selected" : ""}>${esc(t.name)}</option>`),
    ].join("");
    return `
      <div class="map-row" data-activity="${esc(a.id)}">
        <div class="map-act">
          <span class="map-dot" style="background:#${esc(a.color)}"></span>
          <span>${esc(a.name)}</span>
        </div>
        <select class="map-select">${options}</select>
      </div>
    `;
  }).join("");

  rows.querySelectorAll(".map-row").forEach((row) => {
    const activityId = (row as HTMLElement).dataset.activity!;
    const sel = row.querySelector<HTMLSelectElement>(".map-select")!;
    sel.addEventListener("change", () => {
      currentMapping[activityId] = sel.value;
    });
  });
}

async function loadActivityMap() {
  const status = $("activityMapStatus");
  status.style.display = "block";
  status.textContent = "Loading…";
  try {
    const [activities, types] = await Promise.all([
      invoke<ActivityOption[]>("get_early_activities"),
      invoke<YoutrackType[]>("get_youtrack_work_item_types"),
    ]);
    cachedActivities = activities;
    cachedYtTypes = types;
    renderActivityMap();
  } catch (e) {
    status.textContent = `Could not load: ${String(e)}`;
    $("activityMapRows").innerHTML = "";
  }
}

async function openSettings() {
  const s = await invoke<Settings>("get_settings");
  ($("setProvider") as HTMLSelectElement).value = s.provider;
  ($("setEarlyKey") as HTMLInputElement).value = s.early_api_key;
  ($("setEarlySecret") as HTMLInputElement).value = s.early_api_secret;
  ($("setTogglToken") as HTMLInputElement).value = s.toggl_api_token;
  ($("setTarget") as HTMLSelectElement).value = s.target || "jira";
  ($("setJiraUrl") as HTMLInputElement).value = s.jira_base_url;
  ($("setJiraEmail") as HTMLInputElement).value = s.jira_email;
  ($("setJiraToken") as HTMLInputElement).value = s.jira_api_token;
  ($("setYoutrackUrl") as HTMLInputElement).value = s.youtrack_base_url || "";
  ($("setYoutrackToken") as HTMLInputElement).value = s.youtrack_token || "";
  ($("setDefaultIssueKey") as HTMLInputElement).value = s.default_issue_key || "";
  ($("setAutoEnabled") as HTMLInputElement).checked = s.auto_sync_enabled;
  ($("setAutoTime") as HTMLInputElement).value = s.auto_sync_time || "19:00";
  ($("setTrayIcon") as HTMLSelectElement).value = s.tray_icon || "color";
  currentMapping = { ...(s.activity_type_map || {}) };
  toggleProviderFields(s.provider);
  toggleTargetFields(s.target || "jira");
  showView("settingsView");
}

async function saveSettings() {
  const settings: Settings = {
    provider: ($("setProvider") as HTMLSelectElement).value,
    early_api_key: ($("setEarlyKey") as HTMLInputElement).value,
    early_api_secret: ($("setEarlySecret") as HTMLInputElement).value,
    toggl_api_token: ($("setTogglToken") as HTMLInputElement).value,
    target: ($("setTarget") as HTMLSelectElement).value,
    jira_base_url: ($("setJiraUrl") as HTMLInputElement).value,
    jira_email: ($("setJiraEmail") as HTMLInputElement).value,
    jira_api_token: ($("setJiraToken") as HTMLInputElement).value,
    youtrack_base_url: ($("setYoutrackUrl") as HTMLInputElement).value,
    youtrack_token: ($("setYoutrackToken") as HTMLInputElement).value,
    default_issue_key: ($("setDefaultIssueKey") as HTMLInputElement).value.trim(),
    activity_type_map: currentMapping,
    auto_sync_enabled: ($("setAutoEnabled") as HTMLInputElement).checked,
    auto_sync_time: ($("setAutoTime") as HTMLInputElement).value,
    tray_icon: ($("setTrayIcon") as HTMLSelectElement).value,
  };
  try {
    await invoke("save_settings", { settings });
    showView("mainView");
    checkStatus();
    doPreview();
  } catch (e) { alert("Error saving: " + e); }
}

// ── Init ──

window.addEventListener("DOMContentLoaded", () => {
  updateDateLabel();

  $("btnSync").addEventListener("click", doSync);
  $("btnRefresh").addEventListener("click", () => { checkStatus(); doPreview(); });
  $("btnSettings").addEventListener("click", openSettings);
  $("btnBack").addEventListener("click", () => showView("mainView"));
  $("btnSettingsCancel").addEventListener("click", () => showView("mainView"));
  $("btnSettingsSave").addEventListener("click", saveSettings);
  $("prevDay").addEventListener("click", () => shiftDay(-1));
  $("nextDay").addEventListener("click", () => shiftDay(1));
  $("dateLabel").addEventListener("click", toggleCalendar);
  ($("setProvider") as HTMLSelectElement).addEventListener("change", (e) => {
    toggleProviderFields((e.target as HTMLSelectElement).value);
    updateActivityMapVisibility();
  });
  ($("setTarget") as HTMLSelectElement).addEventListener("change", (e) => {
    toggleTargetFields((e.target as HTMLSelectElement).value);
  });

  // Close calendar when clicking outside
  document.addEventListener("click", (e) => {
    if (calendarOpen) {
      const cal = $("calendar");
      const label = $("dateLabel");
      if (!cal.contains(e.target as Node) && !label.contains(e.target as Node)) {
        closeCalendar();
      }
    }
  });

  checkStatus();
  doPreview();

  // Reset to today and refresh whenever panel is opened via tray click
  listen("panel-opened", () => {
    currentDate = new Date();
    calViewYear = currentDate.getFullYear();
    calViewMonth = currentDate.getMonth();
    updateDateLabel();
    closeCalendar();
    checkStatus();
    doPreview();
  });

  listen("show-settings", () => openSettings());
  listen<SyncResponse>("auto-sync-done", (event) => {
    const r = event.payload;
    if (r.synced > 0) {
      new Notification("Synclock", { body: `Auto-synced ${r.synced} entries` });
    }
  });
});
