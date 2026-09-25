import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

const $ = (id: string) => document.getElementById(id)!;
const esc = (s: string | null | undefined) => {
  const d = document.createElement("div");
  d.textContent = s ?? "";
  return d.innerHTML;
};
// Inline `style` attributes are blocked by the CSP, so dynamic colors go through the CSSOM.
const applyDotColors = (root: HTMLElement) => {
  root.querySelectorAll<HTMLElement>("[data-color]").forEach((el) => {
    el.style.background = `#${el.dataset.color}`;
  });
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

type Action = "ok" | "create" | "update" | "delete" | "blocked";
interface PreviewOp {
  entry_id: string; day: string; action: Action; verb: string;
  issue: string; minutes: number; detail: string;
}
interface PreviewItem {
  id: string; activity: string; activity_color: string;
  jira_keys: string[]; duration_min: number;
  started_at: string; stopped_at: string;
  note: string; has_jira_key: boolean; synced: boolean;
  ops: PreviewOp[];
}
interface PlanSummary {
  create: number; update: number; delete: number; ok: number; blocked: number;
  minutes_missing: number; minutes_extra: number;
  deletes_need_confirm: boolean; window_from: string; window_to: string;
}
interface PreviewResponse {
  total: number; with_jira: number; items: PreviewItem[];
  other_ops: PreviewOp[]; summary: PlanSummary;
}
interface SyncResultItem {
  entry_id: string; activity: string; issue_key: string; action: Action;
  duration: string; success: boolean; skipped: boolean; error: string | null;
}
interface SyncResponse {
  synced: number; created: number; updated: number; deleted: number;
  skipped: number; failed: number; deletes_pending: number;
  pending_deletes: PendingDelete[];
  results: SyncResultItem[];
}
interface PendingDelete {
  item_id: string; activity: string; issue: string;
  day: string; minutes: number; reason: string;
}
interface Settings {
  provider: string;
  early_api_key: string; early_api_secret: string;
  toggl_api_token: string;
  target: string;
  jira_base_url: string; jira_email: string; jira_api_token: string;
  youtrack_base_url: string; youtrack_token: string;
  default_issue_key: string;
  activity_type_map: Record<string, string>;
  sync_window_days: number;
  max_deletes_without_confirm: number;
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

const actionTag: Record<Action, string> = {
  ok: "tag-j", create: "tag-c", update: "tag-u", delete: "tag-d", blocked: "tag-d",
};
const fmtDay = (day: string) => day ? new Date(day + "T12:00:00").toLocaleDateString("cs-CZ", { day: "numeric", month: "numeric" }) : "";

const signedDur = (min: number) => `${min < 0 ? "−" : "+"}${fmtDur(Math.abs(min))}`;

function ticketTag(op: PreviewOp) {
  const title = op.action === "ok" ? "Synced" : [op.verb, op.detail].filter(Boolean).join(" · ");
  return op.action === "blocked"
    ? `<span class="tag tag-d" title="${esc(op.detail)}">${esc(op.verb)}</span>`
    : `<span class="tag ${actionTag[op.action]}" title="${esc(title)}">${esc(op.issue)}</span>`;
}

// One readable line per pending change, e.g. "Shorten SIG-523 · 26. 9. · 1h → 45m".
function changeRow(op: PreviewOp) {
  const parts = [
    op.issue ? `<b>${esc(op.issue)}</b>` : "",
    fmtDay(op.day),
    // Shorten/Lengthen already say "1h → 45m" in the detail.
    op.verb === "Shorten" || op.verb === "Lengthen" || op.action === "blocked" ? "" : fmtDur(op.minutes),
    esc(op.detail),
  ].filter(Boolean);
  return `<div class="other-r"><span class="tag ${actionTag[op.action]}">${esc(op.verb)}</span> ${parts.join(" · ")}</div>`;
}

function renderPreview(data: PreviewResponse) {
  $("previewSection").style.display = "block";
  $("logSection").style.display = "none";

  const s = data.summary;
  const totalMin = data.items.reduce((acc, i) => acc + i.duration_min, 0);
  const pending = [
    ...data.items.flatMap((i) => i.ops.filter((o) => o.action !== "ok")),
    ...data.other_ops,
  ].sort((a, b) => a.day.localeCompare(b.day));
  const changes = s.create + s.update + s.delete;
  const net = s.minutes_missing - s.minutes_extra;

  $("summary").innerHTML = [
    `<div class="st"><b>${data.total}</b> entries</div>`,
    `<div class="st"><b>${fmtDur(totalMin)}</b> total</div>`,
    changes ? `<div class="st"><b>${changes}</b> ${changes === 1 ? "change" : "changes"} to sync</div>` : '',
  ].filter(Boolean).join('');

  if (data.items.length === 0) {
    $("entries").innerHTML = '<div class="empty">No entries for this day</div>';
  } else {
    $("entries").innerHTML = data.items.map((item) => {
      const isPending = item.ops.some((o) => o.action !== "ok");
      return `
      <div class="ent${isPending || (item.has_jira_key && !item.synced) ? "" : " ent-dim"}">
        <div class="ent-d" data-color="${esc(item.activity_color)}"></div>
        <div class="ent-b">
          <div class="ent-t">${esc(item.activity)}${item.synced ? '<span class="ent-sd">synced</span>' : ""}</div>
          <div class="ent-s">${fmtTime(item.started_at)} – ${fmtTime(item.stopped_at)}${item.note ? " · " + esc(item.note) : ""}</div>
        </div>
        <div class="ent-r">
          <div class="ent-dur">${fmtDur(item.duration_min)}</div>
          ${item.ops.length ? item.ops.map(ticketTag).join(" ") : item.jira_keys.map((k) => `<span class="tag tag-j">${esc(k)}</span>`).join(" ")}
          ${!item.has_jira_key ? '<span class="tag tag-n">–</span>' : ""}
        </div>
      </div>
    `;
    }).join("");
    applyDotColors($("entries"));
  }

  const scope = s.window_from ? ` (${fmtDay(s.window_from)}–${fmtDay(s.window_to)})` : "";
  $("otherOps").innerHTML = pending.length === 0 ? "" : `
    <div class="other-h">Sync will${scope}:${net ? ` <span title="Change of total logged time in the target">${signedDur(net)} in ${esc($("targetLabel").textContent || "target")}</span>` : ""}</div>
    ${pending.map(changeRow).join("")}
  `;

  ($("btnSync") as HTMLButtonElement).disabled = changes === 0;
  $("syncHint").textContent = changes === 0 && data.items.length > 0
    ? "All synced"
    : s.deletes_need_confirm ? "Deletions will ask for confirmation" : "";
}

// Panel opening, day changes and saving settings can each start a preview;
// only the most recent one may render.
let previewSeq = 0;

async function doPreview() {
  const seq = ++previewSeq;
  const day = dateStr(currentDate);
  const refreshBtn = $("btnRefresh");
  refreshBtn.classList.add("spinning");
  $("previewSection").style.display = "block";
  $("entries").innerHTML = '<div class="empty">Loading...</div>';
  $("otherOps").innerHTML = '';
  $("summary").innerHTML = '';
  $("syncHint").textContent = '';
  ($("btnSync") as HTMLButtonElement).disabled = true;
  try {
    const data = await invoke<PreviewResponse>("preview", { from: day, to: day });
    if (seq === previewSeq) renderPreview(data);
  } catch (e) {
    if (seq === previewSeq) $("entries").innerHTML = `<div class="empty">${esc(String(e))}</div>`;
  } finally {
    if (seq === previewSeq) refreshBtn.classList.remove("spinning");
  }
}

const logLine = (r: SyncResultItem) => {
  const what = `${esc(r.issue_key)} ${r.duration ? "· " + esc(r.duration) : ""}`;
  if (!r.success) return `<div class="l-er">✗ ${esc(r.action)} ${what} ${esc(r.error)}</div>`;
  switch (r.action) {
    case "create": return `<div class="l-ok">✓ Added ${what}</div>`;
    case "update": return `<div class="l-ok">✓ Updated ${what}</div>`;
    case "delete": return `<div class="l-ok">✓ Deleted ${what}</div>`;
    default: return `<div class="l-dm">– ${esc(r.issue_key)} synced</div>`;
  }
};

// Deletions held back by the last sync; confirming approves exactly these.
let pendingDeleteIds: string[] = [];

async function doSync(confirmedDeletes: string[] = []) {
  const day = dateStr(currentDate);
  const btn = $("btnSync") as HTMLButtonElement;
  btn.disabled = true; btn.textContent = "Syncing...";
  $("logSection").style.display = "block";
  $("confirmRow").style.display = "none";
  const log = $("log");
  log.innerHTML = '<div class="l-dm">Starting sync...</div>';

  let pending = 0;
  try {
    const data = await invoke<SyncResponse>("sync", { from: day, to: day, confirmedDeletes });
    // Changes first, unchanged items last.
    const rows = [...data.results].sort((a, b) => Number(a.skipped) - Number(b.skipped));
    let html = rows.map(logLine).join("");
    const parts = [
      `${data.created} added`, `${data.updated} updated`, `${data.deleted} deleted`,
      `${data.skipped} unchanged`, `${data.failed} failed`,
    ];
    html += `<div class="l-dm l-sum">${parts.join(" · ")}</div>`;
    log.innerHTML = html;
    pending = data.deletes_pending;
    pendingDeleteIds = data.pending_deletes.map((d) => d.item_id);
    if (pending > 0) {
      html += `<div class="l-dm l-sum">Awaiting confirmation:</div>` + data.pending_deletes.map((d) =>
        `<div class="l-er">Delete ${esc(d.issue)} · ${fmtDay(d.day)} · ${fmtDur(d.minutes)} · ${esc(d.activity)} · ${esc(d.reason)}</div>`
      ).join("");
      log.innerHTML = html;
      $("confirmHint").textContent = `${pending} work items to delete — review them above.`;
      ($("btnConfirmDelete") as HTMLButtonElement).textContent = `Delete ${pending}`;
      $("confirmRow").style.display = "flex";
    }
    // Keep the log and confirm button visible while deletions await a decision.
    if (pending === 0) setTimeout(() => doPreview(), 500);
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
          <span class="map-dot" data-color="${esc(a.color)}"></span>
          <span>${esc(a.name)}</span>
        </div>
        <select class="map-select">${options}</select>
      </div>
    `;
  }).join("");
  applyDotColors(rows);

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
  ($("setSyncWindowDays") as HTMLInputElement).value = String(s.sync_window_days ?? 14);
  ($("setMaxDeletes") as HTMLInputElement).value = String(s.max_deletes_without_confirm ?? 10);
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
    sync_window_days: Math.max(1, parseInt(($("setSyncWindowDays") as HTMLInputElement).value, 10) || 14),
    max_deletes_without_confirm: (() => {
      const n = parseInt(($("setMaxDeletes") as HTMLInputElement).value, 10);
      return Number.isNaN(n) ? 10 : Math.max(0, n);
    })(),
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

// ── About ──

interface AppInfo { version: string; author: string; repository: string; }

async function renderAbout() {
  try {
    const info = await invoke<AppInfo>("app_info");
    $("about").innerHTML =
      `Synclock v${esc(info.version)} · ${esc(info.author)} · <a id="aboutRepo">GitHub</a>`;
    $("aboutRepo").addEventListener("click", () => openUrl(info.repository));
  } catch { /* purely informational */ }
}

// ── Init ──

window.addEventListener("DOMContentLoaded", () => {
  updateDateLabel();
  renderAbout();

  $("btnSync").addEventListener("click", () => doSync());
  $("btnConfirmDelete").addEventListener("click", () => doSync(pendingDeleteIds));
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
