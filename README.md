# Synclock

A lightweight macOS menubar app that syncs time entries from [Early](https://early.app) (formerly Timeular) or [Toggl Track](https://toggl.com) to Jira worklogs or YouTrack work items.

## Features

- **Menubar app** — lives in the macOS menu bar, no dock icon
- **One-click sync** — preview entries and sync to Jira or YouTrack with automatic deduplication
- **Multiple providers** — supports Early (Timeular) and Toggl Track, switchable in settings
- **Multiple targets** — sync into Jira worklogs or YouTrack work items (switchable in settings)
- **Smart matching** — extracts issue keys from `@PROJ-123` mentions (Early) or descriptions/tags (Toggl)
- **Deduplication** — checks existing worklogs/work items before syncing, safe to run multiple times
- **Two-way consistency (YouTrack)** — edits and deletions in the time tracker are propagated too: changed duration, issue, activity type or day is updated, deleted entries are removed
- **Daily auto-sync** — automatically syncs at a configured time (e.g. 19:00)
- **Right-click menu** — quick sync today, settings, quit

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

1. Click the Synclock icon in the menu bar
2. Navigate days using `‹` `›` arrows or click the date to pick one
3. Review entries — those linked to Jira show a blue issue tag
4. Click **Sync to Jira**

Already-synced entries appear dimmed with a "synced" label. The sync button is disabled when everything is up to date.

With **YouTrack**, the preview tags every issue with what the sync will do — `+` add, `~` update (hover for what changes), `−` delete — and lists changes on other days of the sync window below the entries.

### Keeping YouTrack in line with the tracker

Every YouTrack sync reconciles the whole **sync window** (the last 14 days by default, plus the selected day if it is older), not just the day on screen:

- new entries are added,
- entries whose duration, activity type, day or note changed are updated in place,
- entries moved to another issue are removed from the old issue and added to the new one,
- entries deleted in the tracker — including ones Early deletes silently when another entry is stretched over them — are removed from YouTrack.

Only work items **you** created **through Synclock** (they carry the hidden marker) are ever changed or deleted; work items typed into YouTrack by hand are left alone. An Early activity without a mapped work item type leaves the item's type untouched.

Deletions are guarded: if a sync would delete more than the configured limit (10 by default), or the tracker returned no entries at all for the window, deletions are held back. The panel then shows a **Delete N** button; background syncs send a notification instead. Window size and limit are in Settings → YouTrack.

### Right-click menu

- **Sync Today** — quick-sync without opening the panel
- **Settings** — open the settings view
- **Quit** — exit Synclock

### Daily auto-sync

Enable in Settings → Daily Auto-Sync. Choose a time (e.g. `19:00`) and Synclock will automatically sync the current day's entries once per day at that time. With YouTrack, the auto-sync reconciles the whole sync window, including deletions within the limit above.

## How it works

1. Fetches time entries from Early or Toggl API for the selected day
2. Extracts issue keys from mentions, tags, or descriptions
3. Checks existing worklogs/work items in the selected target to skip duplicates
4. Creates new worklogs via the Jira REST API, or new work items via the YouTrack REST API

**Jira deduplication** matches by start time (±2 min) and duration (±1 min).

**YouTrack deduplication** matches by a hidden marker (`[synclock:{provider}-{entry_id}]`) appended to the work item text — works even if the day already has multiple entries with the same duration and description. The same marker ties each work item back to its entry, which is how edits and deletions are detected: Synclock loads your work items for the window in one request and compares them with the entries.

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
