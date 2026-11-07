# Browser extension spec

Session Manager: a browser extension that keeps tabs in sessions, finds and organizes them, keeps revisions of sessions, and cleans cookies and site data.

Version: 0.13.0. Release notes: [CHANGELOG.md](CHANGELOG.md).

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

## Workspaces
- Allow grouping tabs into workspaces. Workspaces are shared by all sessions; a tab is in at most one.
- Each workspace has a name, a color (the browsers' tab group colors) and an icon.
- Clicking a workspace in the tab list expands its settings. Tabs can be dragged onto a workspace, or moved with the tab editor and the bulk actions.
- Rules add tabs to a workspace automatically when they are opened or change (page, group or tags): when the URL matches a site pattern, when the tab is added to a group (the extension's groups or the browser's native tab groups), when it has a tag, or when it matches a search. The first workspace (in display order) with a matching rule wins. Rules never take a tab out of a workspace, and leave tabs the user placed by hand alone (until the user lets the rules decide again).
- Allow searching in one or more workspaces, or across all workspaces.
- Open a workspace's closed tabs (those listed under its header, or all of them in the session) unloaded and in the background; they keep their workspace.
- Deleting a workspace keeps its tabs, without a workspace.

## Workspaces in the browser
- Optionally mirror workspaces as native tab groups (title = icon and name, the workspace's color), in every window. Moving a tab into a workspace's group (in the same window) puts it in that workspace; moving it out takes it out. Tabs without a workspace keep their own groups. Optionally collapse the groups of workspaces other than the active tab's, when the current workspace changes (groups the user expands or collapses meanwhile stay so).
- One workspace is current: the workspace of the tab last used, or the one switched to. Switching goes to the workspace's most recently used tab. New tabs can join the current workspace.
- The sidebar's tabs can be taken out of their workspace, moved to another, closed (removed, or kept as saved tabs in their session and workspace), or (saved ones) deleted, from buttons and a right-click menu.
- A browser sidebar (Firefox) / side panel (Chrome) lists the window's tabs by workspace, with a workspace switcher, new tabs per workspace, drag and drop between workspaces, and optionally the active session's saved tabs.
- Firefox: optionally hide the tabs of other workspaces than the current one (tabs without a workspace and pinned tabs always show), using the optional `tabHide` permission, which is requested when the option is turned on. Optionally show the current workspace in the window title.
- Firefox: a guide in Settings explains how to hide Firefox's horizontal tab bar with `userChrome.css`: the `about:config` preference to change (`toolkit.legacyUserProfileCustomizations.stylesheets` = `true`), where the file goes, and the stylesheet (copy or download). It opens the pages it mentions where Firefox allows that, and copies their address otherwise.

## Hibernating tabs
- Allow hibernating (unloading) tabs: all browser tabs, a session's, a workspace's, the tabs matching a search, tabs not used for a while, or a single tab. Hibernated tabs stay open and reload when selected.
- Active tabs are never hibernated. Pinned tabs and tabs playing media are skipped, unless hibernation is forced.

## Media playback
- Optionally allow media to play only in the active tab (of each window), or in at most N background tabs besides the active ones. When there are more, the least recently used background tabs are paused.
- Media paused this way resumes when its tab becomes active (configurable).
- Only video counts by default; audio can be included. Picture-in-picture video can keep playing.
- Stopped streams (live streams only: an infinite duration, or a duration or seekable range that keeps moving forward while playing; pausing one with the player's own button, right after user input, stops it too): a tab's streams can be stopped (a button over playing videos, the sidebar, the popup) and resumed. Stopping pauses its media and holds back its media data (by URL: stream manifests, segments, media files; or by response content type). Firefox: blocking webRequest listeners, filtered to the stopped tab, answer when resumed. Chrome: declarativeNetRequest session rules block them. On resume, live streams seek to the live edge, and a page whose playback doesn't recover is reloaded (at most once a minute). A new page in the tab starts with its data flowing. Optionally, a low-resolution loop of the video's last frames (resolution, frame count; copied at most 10 times a second, from the biggest playing video of a frame, while its tab is shown) shows over the stopped video.

## Page guards
- Guards run inside pages (the page's own world, at document start, in every frame) before the pages' own scripts. Each can be turned on or off, and a list of sites (with subdomains) is left alone.
- Pop-ups: block windows and new tabs a page opens without a click, or from handlers that catch clicks anywhere on the page (pop-under ad scripts), including programmatic clicks on links and form submits to new tabs. Links the user clicks keep working.
- Play-button hijacks: during a click on a video player (video, iframe, embed, object, or an element over one) or on an invisible positioned layer over most of the page, scripts can't open windows or new tabs, and a link stretched over the player (or invisible) doesn't open. When such a click tries, the layer it landed on is switched off (pointer-events / visibility) and a paused video under it starts.
- Custom rules: block scripts by URL (declarativeNetRequest; a bare host blocks every script from it; optionally only on given sites), close tabs that pages open to a matching site (and refocus the page), or block every script-opened pop-up on given sites. A log of recent tabs opened by pages offers one-click rules.
- Right-click: pages can't block or replace the context menu. Optionally they can't block selecting and copying text either.
- Debugger traps: `debugger` statements in code built at runtime (`Function`, constructors of functions, string timers) are removed. Firefox: optionally `debugger` statements in script files are blanked out as they load.
- Developer tools detection: pages see a normal window size while the developer tools are docked, can't take away the developer tools shortcuts, and (optionally) their console calls do nothing, which defeats getter / toString probes.
- Replaced functions look native to the page (`Function.prototype.toString`).

## Inspector
- Inspects a tab without the browser's developer tools, through a content script in the extension's isolated world that the page can't see. It runs docked at the bottom of the page by default (an iframe of the extension page in a closed shadow root, resizable; the page gets as much room below its content), or in a separate window. The placement is remembered; a docked inspector is docked again after the page navigates. A docked inspector only runs if the page's dock loaded it (a random token).
- Live: the inspector keeps a port to the content script, which reports DOM mutations of listed nodes and of the selected element; it reconnects after navigations without clearing the view until the new page is read, then selects the same element (by selector) again.
- A docked inspector reloads with its page: a boot script (registered only while a tab is docked) has the background dock it again as soon as the page starts loading, and it restores the tab's saved state (view, selection, network log and recording; storage.session). Firefox gives extension pages framed in web pages only content-script APIs, so the docked inspector there has the background do the rest (tabs, scripting, captures) and relays its page connection.
- Context menus on elements: copy outer / inner HTML, text, the page's selected text within the element, the inspector's own selected text, CSS selector, JS path, XPath, attributes (all or one), computed styles; frames: open in a new tab, copy URL; select, scroll into view, screenshot (save / copy), delete.
- Element screenshots: Firefox renders the element's area of the page (tabs.captureTab with a rect); Chromium stitches screenshots of the visible tab while scrolling (at most two a second). The highlight and the dock are hidden meanwhile, and the scroll position is restored.
- Network: a recorder in the page's own world (web/net-recorder.js; injected on demand, and registered at document start for the sites of recording tabs; a per-tab sessionStorage flag keeps it to the recording tab) wraps fetch, XMLHttpRequest, WebSocket and sendBeacon and reports requests, responses (text bodies up to 256 KB) and WebSocket messages to the inspector content script through a per-session random event name. The log keeps 5000 entries, apart per document (src/netlog.rs). Sockets opened before recording are picked up when the page sends on them.
- Replay (one or a list, with a pause): XHR through a new request carrying the original's handlers and listeners, so the page's code runs again; received WebSocket messages re-dispatched to the socket; sent messages, fetch and beacons sent again. Replays are recorded and linked to the original. Requests of earlier pages in the tab are sent again from what was recorded (URL, method, headers, text body).
- Throttling: rules (site, URL glob, optional method, delay, and a tab or every tab) applied by the background to every request of the site's pages, also after reloads. Rules of a tab are kept for the browser session (storage.session) and removed with the tab; rules of every tab are kept (storage.local). Settings: delay of new rules, pause between replays, scope of new rules, record when the view opens, keep the log across reloads. Firefox: a blocking webRequest listener that answers after the delay. Chrome: the debugger (a required permission: Chrome does not allow it to be optional) is attached to tabs of throttled sites; Fetch interception continues matching requests after the delay.
- Ads: scripts, frames, requests and ad slots of every frame, the ad tags written into the page (inline scripts, decoded base64 payloads) and the tabs the page opened, matched against a catalogue of ad networks (src/ads.rs). Unknown third-party hosts named like ad servers, self-hosted ad-tag shapes, ad-server frames, direct-link redirects and video ad requests are listed as possible ad hosts. Script and close-tab rules in one click.
- Open frame in new tab: a browser context menu item on frames.
- Elements: an element picker (hover highlights, click selects), a lazily loaded DOM tree (including open shadow roots), CSS selector search, and for the selected element its path, a unique selector, size and position, attributes, computed style (filterable), text and HTML; scroll into view and delete.
- Media & frames: the URLs of videos, audio, sources, subtitle tracks, posters, iframes, frames, embeds, objects and scripts in every frame, and media requests from the network log (the streams behind blob: videos), grouped by element type and searchable; copy, open, and block a script.
- Source: the current page source with line numbers and search.

## Media capture
- The inspector's Capture view lists the video and audio elements of the page and all its frames (size, length, playing, source: file, Media Source stream or live stream; protected by DRM), and the stream manifests (HLS, DASH) and media files the page requested. A URL box opens any media URL.
- Recording or saving opens a capture window (capture.html), which does the work and keeps going when the page reloads. Capture windows publish their state (storage.session) for the view, which lists them, brings one to the front, or stops it.
- Element stream: a content script injected on demand into the element's frame (web/capture-page.js, isolated world) records captureStream() / mozCaptureStream() with MediaRecorder: picture and sound, picture only or sound only; WebM, or MP4 where supported; a chosen bitrate. From the start, stop at the end, pause / resume. Listen here: the element is muted in the page and its sound plays in the window. Chunks go to the window, which plays them as they come (Media Source).
- Rendered on screen: the user shares a tab (getDisplayMedia, on a click in the window); the capture is cut to the element's area, which the page reports as it moves (for elements of the top frame and of frames directly in it; the frame's position comes from the top frame's script), fitted for the browser's scaling and letterboxing. Chromium processes the frames off the track (works with the window in the background).
- Protected media (DRM) is not recorded from its stream; captures of it stay as the browser makes them (black). Encrypted HLS and protected DASH streams are not saved.
- Saving a stream: HLS master and media playlists (variants, audio renditions, fMP4 / TS, byte ranges) and DASH manifests (SegmentTemplate with $Number$ / $Time$ and SegmentTimeline, SegmentList, single files) are parsed in Rust (src/stream.rs); the user picks the quality and sound (and DASH subtitles); separate picture and sound make separate files. Parts are fetched in parallel with retries, in the page's frame (as its player does) for streams found in a page, from the window for URLs.
- Recorded WebM files get their duration written into the header (src/webm.rs) so they can be seeked.
- Sinks: save as a file, upload (PUT or POST, raw body or multipart field, custom headers, progress, cancel), play in the window (on a chosen audio output via setSinkId; Firefox: selectAudioOutput), picture-in-picture, full screen. Files stay in the window until saved or uploaded (closing it asks first).

## Search
- Search tabs by keyword, title, URL, host, domain, custom tags, groups and workspace.
- Match by text appearing (contains), exact match, prefix, suffix, glob and regular expression, with optional case sensitivity.
- Optionally search only in certain groups, or across all sessions.
- Support negation and state filters: open, unloaded, hibernated, playing media, pinned, grouped, ungrouped, tagged, duplicate, and not used for a given time.
- Allow selecting several tabs (checkboxes, Ctrl-click, Shift-click ranges, select all, a workspace's tabs) and acting on them: workspace (move in or out), open unloaded, hibernate, tag, group, move to a session, remove from the session, close.
- Allow adding all tabs that match a search to a group, custom tag or workspace (and untagging, moving, hibernating, removing or closing them).

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
- Allow loading a comma separated list of URLs from a text file or the clipboard (also pasted text or a file dropped onto the page). Newlines, spaces and semicolons also separate URLs, and a URL may be enclosed in single or double quotes, which keeps its commas. A CSV / TSV file with a header row is read as a table, and the column that holds the URLs is chosen. Hosts without a scheme get `https://`, duplicates and non-URLs are skipped, and commas inside URLs are kept.
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
- The tab list can be customized: grouped by workspace (default), domain, host, group, tag or not at all; compact rows; tag and group chips shown or hidden. The choice is remembered.
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
- Extensions can't restyle or replace the browser's tab bar; workspaces appear there as native tab groups (or, in Firefox, by hiding tabs). Hiding Firefox's own tab bar takes a `userChrome.css` that the user installs.
- Firefox doesn't let extensions open `about:config` or `about:support`, or change preferences and files in the profile.
- Chrome cannot hide tabs. Tab groups need Chrome / Edge, or Firefox 139+.
- Chrome doesn't let extensions change scripts: `debugger` statements written in script files can only be removed in Firefox.
- Page guards and the inspector don't work on browser-internal pages and extension stores. Guards apply to pages loaded after they are turned on.
- The inspector reads the top frame's DOM; other frames contribute to Media & frames.
- The media limit and the stream controls rely on a content script, registered only while one of them is on; it only runs in pages loaded after that, and not in browser-internal pages. Muted videos smaller than 640×360 (hover previews) are ignored.
- Settings → Performance: switches for the stream controls, the replay, the media limit, the page guards (all off at once) and tab groups. Frequent changes (playback, titles, tab switches, many tabs opened or restored at once, a closing window) are saved together, at most once a second; UI pages parse only the newest state. Snapshots are stored apart (sm_snapshots), written only when they change, and left out of the state sent to pages; tab favicons aren't stored. UI pages search tabs, read large imports (over 64 KB) and write recordings' durations on a worker thread (the same wasm, started on first use, stopped after a minute without work), falling back to the page while it starts or where it can't run; searches run only when the query or the state changes. The popup searches on the page.
- Browsers refuse to hibernate the active tab. Chromium may give a hibernated tab a new id; the extension follows the change.
