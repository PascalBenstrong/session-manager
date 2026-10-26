# Release notes

All notable changes to Session Manager. This file is compiled into the extension and shown on its "What's new" page.

## [0.2.0] - 2026-10-26

### New
- Quick switcher in the toolbar popup: type to search the session, use ↑/↓ and Enter to jump to a tab, Esc to close. Open it with Alt+Shift+S.
- Snapshots: save a session's tab list as a named revision, see what changed since, and restore it (replace, add the missing tabs, or open as a new session). Optional automatic snapshots.
- Save sessions to disk as .json (optionally with snapshots), or the URL list as .txt.
- Import saved sessions, or comma separated URL lists from a file, the clipboard or a paste. Files can also be dropped onto the manager.
- Duplicate tabs: find and remove tabs showing the same page, with adjustable matching, an `is:duplicate` search filter, and an optional "prevent duplicates" mode.
- "What's new" page with these release notes, and a NEW badge on the toolbar icon after an update.

### Improved
- Modern look with dark and light themes (follows the system, or pick one).
- The full manager has a sidebar with all sessions and views.
- Tab rows show the site, open/unloaded state, group and tags. Actions appear on hover.
- Search options and bulk actions stay out of the way until needed.
- Notifications appear as small toasts.

### Fixed
- Clicking anywhere on a tab row now switches to the tab (previously only the title text was clickable).
- Switching to a tab whose browser tab had gone away reopens it instead of failing.

## [0.1.0] - 2026-10-26

### New
- Sessions: open tabs are added to the active session automatically, and closing a tab removes it. Tabs of a closed window are kept unloaded.
- Switch the active session without loading its tabs. Load a session's tabs unloaded, in a new window, or fully. Find the last used tab of a session.
- Search by keyword, title, URL, host, domain, tag or group, with exact, prefix, suffix, glob and regex matching, negation and state filters. Results can be limited to certain groups.
- Tag, group, move or remove every tab that matches a search.
- Re-organize browser tabs by domain, host, group or tag, optionally as native tab groups and automatically. Rules tag and group tabs by URL pattern.
- Clean cookies, cache and site storage per host, domain, URL prefix, glob or regex, for all open tabs, or for all domains, with sites to keep. Rules can run on an interval or remove cookies as soon as a site sets them.
- Firefox and Chrome builds from one Rust + egui + WebAssembly code base.
