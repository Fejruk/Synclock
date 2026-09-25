# Synclock

A lightweight macOS menubar app that syncs time entries from [Early](https://early.app) (formerly Timeular) or [Toggl Track](https://toggl.com) to Jira worklogs or YouTrack work items.

## Features

- **Menubar app** — lives in the macOS menu bar, no dock icon
- **One-click sync** — preview entries and sync to Jira or YouTrack with automatic deduplication
- **Multiple providers** — supports Early (Timeular) and Toggl Track, switchable in settings
- **Multiple targets** — sync into Jira worklogs or YouTrack work items (switchable in settings)
- **Smart matching** — extracts issue keys from `@PROJ-123` mentions (Early) or descriptions/tags (Toggl)
- **Deduplication** — checks existing worklogs/work items before syncing, safe to run multiple times
- **Stays in sync (YouTrack)** — edits and deletions in the time tracker are propagated too: changed duration, issue, activity type or day is updated, deleted entries are removed
- **Daily auto-sync** — automatically syncs at a configured time (e.g. 19:00)
- **Left-click panel, right-click menu** — quick sync today, settings, quit

## Installation

### Download

Download the latest `.dmg` from [Releases](https://github.com/Fejruk/Synclock/releases) and drag **Synclock.app** to Applications.

The app is not code-signed, so macOS will block it on first launch. To fix this, run once in Terminal:

```bash
xattr -cr /Applications/Synclock.app
```

Then open Synclock normally.

### Build from source

Requires [Rust](https://rustup.rs/) and [Node.js](https://nodejs.org/) (v18+).

```bash
git clone https://github.com/Fejruk/Synclock.git
cd synclock
npm install
npm run tauri build
```

The built app will be at `src-tauri/target/release/bundle/macos/Synclock.app`.

## Setup

1. Launch Synclock — a sync icon appears in the menu bar
2. Click the icon, then the gear icon to open **Settings**
3. Configure your time tracking provider and Jira credentials

### Early (Timeular)

- **API Key** + **API Secret** — generate at [early.app](https://early.app) → Profile → API Access

### Toggl Track

- **API Token** — find at [Toggl Profile](https://track.toggl.com/profile)

### Target tracker

Pick **Jira** or **YouTrack** in Settings → Target Tracker. Both configs persist in parallel, so you can switch back and forth without re-entering credentials.

### Jira

- **Base URL** — your Jira instance (e.g. `https://yoursite.atlassian.net`)
- **Email** — your Atlassian account email
- **API Token** — generate at [Atlassian API Tokens](https://id.atlassian.com/manage-profile/security/api-tokens)
  - Required scopes: `read:jira-user`, `read:jira-work`, `write:jira-work`

### YouTrack

- **Base URL** — your YouTrack Cloud instance (e.g. `https://yourcompany.youtrack.cloud`)
- **Permanent Token** — generate in YouTrack: Profile → Account Security → New permanent token

YouTrack work items created by Synclock include a hidden marker (e.g. `[synclock:toggl-12345]`) at the end of the text. This is how the app detects already-synced entries on subsequent syncs. Don't remove the marker if you want dedup to keep working — editing the rest of the text is fine.

#### Activity → YouTrack work item type (Early only)

When YouTrack is the target and Early is the provider, Settings shows an **Activity → YouTrack Type** section listing your Early activities. Each row maps an activity to a YouTrack work item type (Development, Testing, Meeting, …) — pick `(no type)` to leave the type empty. Synclock applies the mapped type when creating work items, so your YouTrack reports stay grouped the same way as your Early activities.

## Usage

### Linking time entries to Jira issues

**Early:** Type `@PROJ-123` in the time entry notes. Early creates a mention that Synclock picks up automatically.

**Toggl:** Include the issue key anywhere in the description (e.g. `PROJ-123 Standup`) or add it as a tag.

### Syncing

1. **Left-click** the Synclock icon in the menu bar to open the panel
2. Navigate days using `‹` `›` arrows or click the date to pick one
3. Review entries — each shows its issue tag, colored by what the next sync will do
4. Click **Sync to Jira** / **Sync to YouTrack**

Already-synced entries appear dimmed with a "synced" label. The sync button is disabled when everything is up to date ("All synced").

### Reading the YouTrack preview

The top row describes the **selected day**: number of entries, total time, and how many changes a sync would make (across the whole sync window, see below).

Issue tags on entries are colored by the pending action — blue: in sync, green: will be added, yellow: will be updated, red: will be deleted. Hover a tag for details.

Below the entries, **Sync will (from–to):** lists every pending change in the sync window, one line each, including changes on other days and work items of deleted entries:

| Line | Meaning |
|---|---|
| `Add SIG-523 · 26. 9. · 30m` | New entry — a work item will be created |
| `Shorten SIG-523 · 26. 9. · 30m → 15m` | Entry got shorter — the work item's duration is reduced |
| `Lengthen SIG-523 · 26. 9. · 30m → 45m` | Entry got longer — the work item's duration is increased |
| `Move SIG-523 · 24. 9. · 30m · moved 26. 9. → 24. 9.` | Entry moved to another day — the work item's date changes (the day shown is the new one) |
| `Update SIG-523 · 26. 9. · 30m · note` | Note or activity type changed (or several things at once — all are listed) |
| `Delete SIG-523 · 26. 9. · 30m · entry deleted` | Entry no longer exists — its work item will be removed |
| `Delete SIG-523 · … · ticket changed` | Entry now points to another issue — removed here, added there (a separate `Add` line) |
| `Delete SIG-523 · … · duplicate` | Two work items belong to the same entry and issue — one is kept |
| `Can't sync` | The issue key could not be resolved in YouTrack; this entry and its work items are left untouched |

The header also shows the net effect on logged time, e.g. `−15m in YouTrack` (omitted when it evens out).

### Keeping YouTrack in line with the tracker

With YouTrack as the target, Synclock does not just add new time — it keeps YouTrack **identical** to Early/Toggl: whatever you change or delete in the time tracker is changed or deleted in YouTrack on the next sync.

#### The sync window

Every YouTrack sync reconciles the whole **sync window** — the last **14 days** up to today by default (Settings → YouTrack → *Sync window*, 1–90 days) — not just the day on screen. If you pick a day older than that, the window stretches back to include it. Manual sync, **Sync Today** and the daily auto-sync all use the same window.

On top of the window, Synclock also loads **31 days on each side** as a safety buffer. Entries in the buffer are never *added* to YouTrack, but if one of them already has a work item, that work item keeps following it (so an entry moved just outside the window is updated, not deleted).

#### What happens when you…

| You do in Early/Toggl | Next sync in YouTrack |
|---|---|
| create an entry with an issue key | work item **added** |
| shorten / lengthen it | same work item, **duration updated** |
| edit the note | same work item, **text updated** |
| change the activity (Early) | same work item, **type updated** — only if the new activity has a mapped work item type; unmapped activities leave the type as it is |
| change only the start/end time within the same day | nothing — YouTrack stores the day, not the time (duration changes still apply) |
| **move it to another day** | same work item, **date updated** (`Move`) — no duplicate is created |
| change the issue key | work item **deleted** on the old issue, **added** on the new one |
| add a second issue key | the time is split between the issues: the existing work item shrinks, a new one is added |
| remove the issue key | work item **deleted** (or moved to the default task if one is configured) |
| delete the entry | work item **deleted** |

#### Moving an entry to another day — details

The work item is matched to its entry by the hidden marker, not by date, so a moved entry keeps its work item:

- **New day inside the window** → the work item's date is changed. Nothing is added or removed.
- **New day older than the window, but within the 31-day buffer** → still just a date change.
- **New day even further back** (roughly more than 45 days ago with the default window) → the entry is out of reach, so the work item left in the window looks orphaned and is **deleted**. The entry is logged again on its new day as soon as you select that day in the panel and click Sync (the window stretches to it).
- **New day in the future** → handled the same way (the buffer covers 31 days ahead).

> **Watch out for overlaps in Early.** When you move or stretch an entry over another one, Early silently deletes the entry underneath. Synclock mirrors Early, so the work item of that deleted entry is removed from YouTrack too. Check the preview for unexpected `Delete … entry deleted` lines before syncing.

#### What is never touched

- Work items you entered in YouTrack **by hand** (they have no Synclock marker).
- Work items of **other people** — only your own work items are loaded, and the author is checked again.
- Work items created from **another provider** (switching Early ↔ Toggl never deletes the other one's items).
- Work items **outside the window** whose entry cannot be found — only items inside the window are ever considered orphaned.

#### Safety limits for deletions

Deletions run automatically only when they look routine. A sync **holds deletions back** when:

- it would delete more than the limit (Settings → YouTrack → *Deletions without confirmation*, default **10**), or
- the time tracker returned **no entries at all** for the window (more likely an outage than a cleared fortnight).

Adds and updates still go through. In the panel, the log then lists the held-back work items and a **Delete N** button deletes exactly those — nothing a fresh sync might have found in the meantime. Background syncs (auto-sync, Sync Today) send a notification instead.

If loading entries or work items fails, the sync stops without changing anything. If one operation fails, the error is shown in the log and the rest continue; the next sync retries it.

### Menu bar icon

- **Left-click** — open or close the panel
- **Right-click** (or Ctrl-click) — menu:
  - **Sync Today** — quick-sync without opening the panel
  - **Settings** — open the settings view
  - **Quit** — exit Synclock

### Daily auto-sync

Enable in Settings → Daily Auto-Sync. Choose a time (e.g. `19:00`) and Synclock will automatically sync once per day at that time. With Jira it syncs the current day; with YouTrack it reconciles the whole sync window, including deletions within the limit above.

## How it works

1. Fetches time entries from Early or Toggl (for YouTrack: the sync window plus buffer)
2. Extracts issue keys from mentions, tags, or descriptions (falling back to the default task, if set)
3. Loads existing worklogs/work items from the target
4. Jira: creates the worklogs that are missing. YouTrack: compares entries with work items and creates, updates or deletes work items to match

**Jira deduplication** matches by start time (±2 min) and duration (±1 min). Jira sync only ever adds worklogs; edits and deletions are not propagated.

**YouTrack matching** uses a hidden marker (`[synclock:{provider}-{entry_id}]`) appended to the work item text. It ties each work item to exactly one time entry, so it works even with several identical entries on one day, and it is what lets Synclock recognize edited, moved and deleted entries. All of your work items for the window are loaded in a single paginated request (`/api/workItems?author=me`); issue keys that are not yet known (new issues, legacy aliases migrated from Jira) are resolved in parallel.

## Configuration

Credentials are stored locally at:

```
~/Library/Application Support/synclock/config.json
```

No credentials are stored in the app bundle or source code.

## Tech stack

- [Tauri v2](https://v2.tauri.app) — native macOS app framework
- Rust — backend API calls and sync logic
- TypeScript + HTML — frontend UI
- Vite — build tooling

## License

MIT
