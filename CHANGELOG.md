# Release notes

All notable changes to Session Manager. This file is compiled into the extension and shown on its "What's new" page.

## [0.6.0] - 2026-10-27

### New
- Stop ads that hijack the play button (Page guards): clicks on a video player, or on an invisible layer over the page, can't open windows or tabs. A layer laid over the player to catch your click is switched off, so the click starts the video instead.
- Open frame in new tab: right-click any iframe or frame in a page.
- Inspector, Ads: the ad networks a page runs (pop-up networks, display, native, video, exchanges, verification), found in the scripts, frames, requests and ad slots of all its frames and in the tabs it opened, with one-click rules to block a network on the site or everywhere.
- The inspector opens docked at the bottom of the page: 🗗 moves it to its own window, ⬓ docks it again; the choice is remembered, and a docked inspector comes back when the page reloads or navigates. Drag its top edge to resize it.
- The inspector is live: the element tree and the selected element's details follow changes to the page, and it reconnects by itself after a reload.
- Selected elements stay highlighted on the page, with their name and size, and the highlight follows them as the page scrolls.
- Right-click menus in the inspector: copy an element's HTML, inner HTML, text, the text selected on the page or in the inspector, its CSS selector, JS path or XPath, its attributes or computed styles; open or copy a frame's URL; scroll to, select, photograph or delete it. Attribute, style, HTML and source lines have their own copy menus.
- Element screenshots: save a PNG of an element, or copy it, even when it's taller than the window.

## [0.5.0] - 2026-10-27

### New
- Page guards (a new view): protections that run inside pages before the pages' own scripts, with a list of sites to leave alone.
- Pop-ups and ad tabs: block windows and tabs a page opens without a click, or from code that catches clicks anywhere on the page (pop-under ad scripts).
- Custom rules: block a script by its URL (only on chosen sites if you like), close tabs that pages open to an ad site, or block every pop-up on a site. A list of tabs opened by pages turns an ad tab into a rule in one click.
- Always allow right-click, and optionally selecting and copying text, on pages that block them.
- Remove debugger traps: `debugger` statements that pages build at runtime no longer freeze the page when the developer tools are open. In Firefox, also those in script files.
- Hide the developer tools from pages that look for them (window size, console tricks, blocked shortcuts).
- Inspector: inspect any page in a separate window, without the browser's developer tools. Pick elements, browse the DOM, and see attributes, size, computed styles and HTML. Search with CSS selectors, delete elements, and read the page source. Open it with Alt+Shift+I, 🔬 in the popup, or from a tab.
- Media & frames: every video, audio, source, subtitle, iframe, frame, embed and script URL of the page and its frames, plus the streams behind blob: videos. Grouped by element type and searchable, with copy, open and "block this script".

## [0.4.0] - 2026-10-27

### New
- Workspaces as tab groups: each workspace's tabs sit in a native tab group with its icon, name and color. Dragging a tab into or out of a group moves it between workspaces. Optionally, only the group of the workspace you're in stays open.
- Workspace sidebar (Firefox sidebar, Chrome side panel): the window's tabs listed by workspace, a workspace switcher, and new tabs per workspace. Open it with Alt+Shift+W or from the popup.
- Switch workspaces from the sidebar, the Tabs view or a workspace's settings. The current workspace follows the tab you use, and new tabs can join it.
- Firefox: hide the tabs of other workspaces, so the tab bar only shows the current workspace. Optionally show the workspace in the window title.
- Firefox: a step-by-step guide in Settings to hide Firefox's own tab bar with userChrome.css, naming the preference to change and providing the stylesheet to copy or download.

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
