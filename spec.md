# Browser extension spec

Session Manager: a browser extension that keeps tabs in sessions, finds and organizes them, keeps revisions of sessions, and cleans cookies and site data.

Version: 0.2.0. Release notes: [CHANGELOG.md](CHANGELOG.md).

## Platform and architecture
- Supports Firefox and Chrome (Chromium-based browsers) with Manifest V3.
- Written in Rust; the UI uses egui; both compile to WebAssembly.
- Shared/core code lives in the project root (`src/`, `web/`, `icons/`). Browser-specific files live in a folder per browser (`firefox/`, `chrome/`): the manifest and a small JS shim for API differences.
- Browser-independent logic (sessions, search, organizing, duplicates, snapshots, import parsing, cleanup planning) is plain Rust with unit tests.
- Sessions and settings are stored locally in the browser (`storage.local`). Nothing is sent anywhere.

## Sessions and tabs
- Automatically add open tabs to the current (active) session.
- Closing a tab removes it from the session. When a whole window closes or the browser quits, its tabs stay in the session as unloaded (configurable).
- Allow manually removing tabs from a session. A removed tab stays open but is no longer tracked.
- Allow setting a session as active without loading all the tabs in it.
- Allow loading the tabs of a session unloaded (lazily), in a new window, or fully loaded.
- Allow finding the last open (most recently used) tab in a session.
- Create, rename and delete sessions, and move tabs between sessions.
- After a browser restart, re-link saved tabs to the open tabs.

## Search
- Search tabs by keyword, title, URL, host, domain, custom tags and groups.
- Match by text appearing (contains), exact match, prefix, suffix, glob and regular expression, with optional case sensitivity.
- Optionally search only in certain groups, or across all sessions.
- Support negation and state filters: open, unloaded, pinned, grouped, ungrouped, tagged, duplicate.
- Allow adding all tabs that match a search to a group or custom tag (and untagging, moving, removing or closing them).

## Organizing
- Allow re-organizing browser tabs by domain/host, custom tags or groups, optionally as native tab groups where the browser supports them.
- Allow re-organizing automatically when tabs change.
- Rules that automatically tag and group tabs whose URL matches a pattern.

## Duplicate tabs
- Find tabs that show the same page, in the current session or across all sessions.
- Configurable comparison: ignore `#fragment`, `?query`, a trailing `/`, `www.`, and http vs https.
- Remove duplicates in one action. Keep one copy (prefer the open, most recently used one) and merge the tags and group of the removed copies into it. Close the extra open tabs, or optionally only remove them from the session.
- Optionally prevent duplicates: opening a page already in the active session switches to the existing tab. If the saved copy is unloaded, the new tab takes its place. Tabs the extension opens itself are never affected.

## Snapshots (revisions)
- Allow saving revisions/snapshots of a session's tab list, with an optional name.
- Show each snapshot's date, size and difference from the current session.
- Restore a snapshot by replacing the session's tabs (the current list is snapshotted first, so a restore can be undone), by adding only the missing tabs, or as a new session. Optionally open the restored tabs unloaded.
- Optional automatic snapshots on an interval, only for sessions that changed, keeping the last N. Manual snapshots are never deleted automatically.

## Saving and importing
- Allow saving sessions to disk: one session or all sessions as a file (optionally including snapshots), a single snapshot, or the URL list as text.
- Allow loading a comma separated list of URLs from a text file or the clipboard (also pasted text or a file dropped onto the page). Newlines, spaces and semicolons also separate URLs. Hosts without a scheme get `https://`, duplicates and non-URLs are skipped, and commas inside URLs are kept.
- Imported URLs go into the current session or a new one, optionally opened unloaded.
- Importing a saved session file adds its sessions as new sessions and never overwrites existing ones.

## Cookie/browser session management
- Allow cleaning the browser cache, cookies and site storage (localStorage, IndexedDB, service workers) per host, domain, URL prefix or pattern (glob, regex).
- Allow automatic clean-up on an interval.
- Allow clean-up on all open tabs.
- Allow clean-up for all domains.
- Allow clean-up whenever a new value is set (cookies are removed as soon as a matching site sets them).
- Allow keeping (excluding) chosen domains from clean-up.
- Save clean-ups as rules (enable/disable, run now, edit) and keep a log of recent clean-ups.

## User interface
- **Toolbar popup:** a mini quick switcher; no full page is needed to find a tab.
  - Open it from the toolbar icon or a keyboard shortcut (default Alt+Shift+S).
  - Type to search the session. ↑/↓ moves, Enter or click jumps to the tab (and closes the popup), Esc closes.
  - Includes the session picker, quick session actions, and links to the full manager and the release notes.
- **Full manager page:**
  - A sidebar with all sessions and the views: Tabs, Organize, Snapshots & import, Cookies & data, Settings, What's new.
  - A compact layout on narrow windows.
- Modern look with dark and light themes. Follows the system by default and can be switched; the choice is remembered.
- Clicking anywhere on a tab row switches to that tab, or opens it if it is unloaded.
- Destructive actions ask for confirmation. Results and errors appear as notifications.

## Release notes
- Release notes are included in the extension and shown on a "What's new" page.
- After an install or update, the toolbar icon shows a NEW badge and the popup highlights "What's new". Both clear once the notes have been viewed.
- The versions in the changelog, the Rust package and both manifests must match.

## Known platform limitations
- Firefox cannot clear the HTTP cache for individual sites. Per-site clean-ups skip the cache there with a warning; all-domains clean-up clears it.
- Chromium cannot create tabs already unloaded, so they are created and then discarded immediately.
- "Clean up whenever a new value is set" applies to cookies only. Other storage has no change events extensions can observe.
- File pickers close extension popups, so saving and importing happen in the manager page.
- Grouping by domain uses a heuristic registrable domain (common `co.uk`-style suffixes), not the full Public Suffix List.
