# Release notes

All notable changes to Session Manager. This file is compiled into the extension and shown on its "What's new" page.

## [0.3.0] - 2026-10-27

### New
- Workspaces: group tabs into named workspaces with a color and an icon. They are shared by all sessions, and the Tabs view lists tabs by workspace.
- Click a workspace's header to open its settings: name, icon, color, rules, and actions. Drag tabs onto a header to move them there.
- Workspace rules add tabs automatically when they open or change: by site (domain, host, URL prefix, glob or regex), by group (including the browser's own tab groups), by tag, or by any search. Tabs you placed by hand stay where you put them.
- Search in one workspace or across all of them: a workspace filter in the search options, and `ws:name` in the search box.
- Hibernate tabs to free memory: all tabs, this session's, a workspace's, the tabs matching a search, or tabs not used for a while. "Force hibernate" includes pinned tabs and tabs playing media. Hibernated tabs stay in the tab strip and are marked in the list.
- Media limit: let videos play only in the active tab, or in at most N background tabs. Paused media resumes when you switch back to its tab. Audio and picture-in-picture can be included or exempt.
- New search filters: `is:hibernated`, `is:playing` and `idle:30m` / `idle:2h` / `idle:1d`.

### Improved
- Customize the tab list from the View menu: group by workspace, domain, host, group, tag or nothing; compact one-line rows; show or hide tag and group chips. The choice is remembered.
- The quick switcher shows each tab's workspace and whether it is hibernated, and can hibernate a tab.

## [0.2.0] - 2026-10-26

### New
- Quick switcher in the toolbar popup: type to search the session, use ↑/↓ and Enter to jump to a tab, Esc to close. Open it with Alt+Shift+S.
- Snapshots: save a session's tab list as a named revision, see what changed since, and restore it (replace, add the missing tabs, or open as a new session). Optional automatic snapshots.
- Save sessions to disk as .json (optionally with snapshots), or the URL list as .txt.
- Import saved sessions, or comma separated URL lists from a file, the clipboard or a paste. Files can also be dropped onto the manager.
- Duplicate tabs: find and remove tabs showing the same page, with adjustable matching, an `is:duplicate` search filter, and an optional "prevent duplicates" mode.
- "What's new" page with these release notes, and a NEW badge on the toolbar icon after an update.
- Clear cookies & site data from the toolbar popup: for the current tab (🧹 menu, cookies only or everything, optionally reloading the tab), or for any open tab in the list.
- Remove duplicate tabs automatically when the browser restarts and restores its tabs, and/or when a session's tabs are reopened. Tabs restored after a restart go back to the session they were saved in.

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
