# Performance goals

Work list from a CPU and memory audit done on 2026-11-01 against commit `a053cac` (0.11.4).
Each goal says where the cost is, why it matters, how to fix it, and how to tell it is done.
Line numbers are from that commit and may have moved. Search for the quoted code if they don't match.

Goals marked **verified** were checked by reading the code. The others came from the audit
and still need checking. Read the code before changing it, and drop a goal if the problem isn't there.

## Ground rules

- Behavior must not change. These are performance fixes only.
- Keep each goal in its own commit, so a regression can be traced and reverted.
- Match the style around the code you change (naming, comment density, idioms).
- Build: `PROFILE=dev ./build.sh firefox` (needs `wasm32-unknown-unknown` and the matching `wasm-bindgen-cli`).
- Test the pure core natively with `cargo test`. Test the background logic end to end with
  `./build.sh firefox && node tools/smoke-test.mjs firefox` (and `chrome`).
- Sizes to keep in mind: 500+ open tabs, 2,000+ saved entries, 10 workspaces,
  5,000 network log entries with bodies up to 256 KB, and video streams of several hundred MB.
- Don't redo work from 0.11.1–0.11.4. That includes: the 1 s debounce on playback, title and activation saves;
  the media.js pointermove early return; hit-testing only playing videos; 10 fps replay copies; and freeing
  frames on `emptied`.

Common pattern for the UI goals: egui redraws on every input event, so anything inside `update`/`ui`
runs many times a second. Compute derived data when its source changes (key it on `state.revision`,
a query string, an entry id, and so on), store it on the view struct, and only borrow it while drawing.

---

## P1: high impact

### G1. Stop cloning the whole app state every frame (verified)
- **Where:** `src/ui/mod.rs:947` `self.state.clone()`, `:954` `state.session(viewed).cloned()`.
- **Why:** copies every session, entry, media item and log on every frame, on every page. The sidebar is always open.
- **Fix:** store the state as `Option<Rc<AppState>>` and clone the `Rc`, or borrow it for the frame. Borrow the session instead of cloning it.
- **Done when:** no `AppState` or `Session` deep copy happens per frame. The sidebar and manager look and behave the same.

### G2. Network view: stop cloning the filtered log and re-rendering bodies every frame (verified)
- **Where:**
  - `src/ui/network_view.rs:363` and `:461`: `self.log.filtered(..).cloned().collect()`, twice per frame.
  - `:411`: clones the selected entry.
  - `:940`: parses the response body as JSON and pretty-prints it, every frame.
  - `src/netlog.rs:252` `filtered()`: builds a `format!(..).to_lowercase()` per entry.
- **Fix:**
  - Store `Vec<usize>` of matching indices, keyed on `(log revision, query, kind)`, and share it between the toolbar and `list()`.
  - Rows borrow `&self.log.entries[i]`. Only copy out the clicked id.
  - Store the pretty-printed body per entry id. Show only about the first 50 KB, or split it into lines and draw them with `ScrollArea::show_rows`.
  - Store a lowercased search string per entry, built once.
- **Done when:** an idle frame with 5,000 entries allocates nothing in proportion to the log size.

### G3. Inspector panes: no per-frame clones or recomputation
- **Where:** `src/ui/inspector_view.rs`:
  - `:1140` `self.details.clone()`, which includes `outer_html` up to 200K characters. The HTML is drawn as one wrapped label (`:1258`).
  - `:1576-1584` `self.source.clone()` (a `Vec<String>` of the whole page source), plus lowercasing every line per frame when filtered.
  - `:921`, `:970`: the DOM tree `self.root.clone()`, plus `children.get(..).cloned()` per expanded node.
  - `:1401-1415`: the Ads pane clones its signals and calls `ads::providers()` every frame.
  - `:1269-1286`: the Media & frames pane clones and filters `resources` every frame.
  - `:1214-1235`: strings for the copy menu are built for every row, every frame.
- **Fix:**
  - Borrow the data, using `Option::take` and putting it back, or `Rc`, instead of cloning.
  - Store filter results and `providers()` output, and rebuild them only when their inputs change.
  - Truncate or virtualize the HTML preview.
  - Build the copy strings inside the `context_menu` closure.
- **Done when:** with `<body>` selected and the mouse moving, frame time doesn't grow with the page's size.

### G4. Tab lookups: stop scanning every entry for each tab
- **Where:**
  - `src/state.rs:70` `locate_tab` and `:84` `entry_for_tab` search every entry of every session.
  - `src/tab_strip.rs:85`, `:96`, `:145` call them inside window × workspace × tab loops.
  - The same lookup runs per tab in `src/background.rs:1276` (`tracked`) and `:1362` (`organize::plan`).
- **Why:** this runs up to every 300 ms while pages load, about 20 million comparisons at the sizes above.
- **Fix:** build a `HashMap<TabId, (EntryRef, Option<WorkspaceId>)>` once per sync and pass it in. Better: keep that index in `AppState` (not serialized) and update it in `on_tab_upsert`, `on_tab_removed` and `link_tab`.
- **Done when:** a strip sync does work in proportion to tabs + entries. Add a unit test that the index stays correct across create, close and link.

### G5. Cookie listener: do nothing when no rule needs it (verified)
- **Where:** `src/background.rs:534-542` `on_cookie_set`. It copies `open_urls` for every cookie event and rebuilds `SpecMatcher::new` (`src/cleanup.rs:188`, which compiles the regex or glob) for each matching rule.
- **Fix:**
  1. Return early when no enabled rule has `on_cookie_set`, before calling `open_urls`.
  2. Store the compiled matchers and rebuild them only when the rules or the set of open tabs change.
  3. Best: in `web/platform-common.js`, register `cookies.onChanged` only while such a rule exists.
- **Done when:** a page that sets 100 cookies costs close to nothing when no cookie-set rule exists.

### G6. Network recorder: cap memory and do no work when not recording (verified)
- **Where:** `web/net-recorder.js`:
  - `:43`, `:50-53`: `kept` holds up to 1000 entries with no byte cap.
  - `:191`: `keep(id, { kind: 'xhr', xhr: self, ... })` keeps the live XHR and its whole `response`.
  - `:285`: WebSocket binary frames are kept.
  - `:123`: `Request` clones are kept.
  - `:131-158`, `:193-214`: the wrappers do all their work even when `recording` is off (only `emit()` checks it, `:60`).
  - `:207`: `stringify(self.response)` runs on the full response before `clip()`.
- **Fix:**
  - At `loadend`, copy only what `replayXhr` reads (`withCredentials`, `responseType`, `timeout`, the `on*` handlers, a copy of the listeners) and drop the XHR reference.
  - Add a total-bytes budget for kept bodies and frames, and skip binary frames over about 64 KB.
  - Check `recording` (or an active replay) at the top of each wrapper and pass straight through when it's off.
  - Skip stringifying when the response is larger than about 4× the cap.
- **Done when:** recording a page that plays an HLS video keeps memory bounded. After stopping, wrappers add no work.

### G7. Stop page loops that never end
- **G7a.** In `web/capture-page.js:269-300` and `web/capture-engine.js:85-127,439-452`, `watchRect`/`watchFrame` loops (rAF + `getBoundingClientRect` + 2× `getComputedStyle` + `JSON.stringify`, plus a 500 ms interval) are never stopped after a cropped recording. A second `watchRect` call starts new loops without clearing the old ones.
  - **Fix:** add an `unwatch` operation called from `r.cleanup`. Cancel any existing rAF and interval in `follow()` before starting new ones.
- **G7b.** In `web/capture-page.js:239-244,294-296`, nested frames send a "locate" greeting every 2 s forever. Each one makes the top frame run `deepAll('iframe, frame')`, a scan of every element and shadow root.
  - **Fix:** stop sending once the top frame has answered. In the top frame, return early when the token is already located and its frame is still connected.
- **G7c.** In `web/media.js:383-430`, with replay on, a `requestVideoFrameCallback` loop runs for every playing video, not only live ones.
  - **Fix:** start capturing only once `isLive(v)` is true. Stop re-arming for videos that aren't live and for previews.
- **G7d.** In `web/inspect.js:228-242`, the highlight follower forces layout and restyles on every frame.
  - **Fix:** write only when the rect changes, or update on scroll/resize/`ResizeObserver` events instead of rAF.
- **Done when:** no rAF or interval keeps running after its feature is done. Check with the browser profiler.

### G8. Cache duplicate detection
- **Where:** `src/ui/tabs_view.rs:959` `duplicates_badge` calls `find_duplicates` every frame (it builds a normalized key String per entry and a `HashMap<String, Vec<_>>`). It runs across all sessions when "All sessions" is on. The same happens in `organize_view.rs:218` and `mod.rs:683`.
- **Fix:** store the result keyed on `(state.revision, scope, dedupe options)`.

---

## P2: medium impact

### G9. Batch full-state saves
- **Where:** `src/background.rs` `persist()` (`:229`) is called immediately, with no delay, from:
  - `TabRemoved` (`:405`)
  - `TabCreated` for loaded tabs (`:367`)
  - `check_opened_tab` (`:209`)
  - `enforce_media` (`:1215`)
  - `stop_stream` (`:1166`)
- **Why:** closing 100 tabs means 100 full writes, and every open sidebar or popup parses each one.
- **Fix:** route these through `schedule_persist`, or add a "persist soon" that combines writes within about 100 ms. Have `persist()` clear `PERSIST_PENDING`, so a delayed save doesn't write the same state again.

### G10. Don't send the full state in command replies
- **Where:** `src/background.rs:627` `execute()` clones the state (`snapshot()`) into every reply, and `persist()` sends it again through storage.
- **Fix:** when the command already persisted, reply with only the revision number. Pages already pick up the state from the storage change. Otherwise serialize straight from the borrowed state.
- **Check:** confirm every page updates from the storage change before removing the state from replies.

### G11. Network log: store each body once
- **Where:** `src/netlog.rs:128`, `:176-184`. Each record keeps both the raw `Map` and the parsed `NetEntry`, and clones the merged map on every update. Once the log is full, `:198` rebuilds the 5,000-entry `index` on every batch.
- **Fix:** keep bodies once (in `Rc<str>`, or rebuild the export from `NetEntry`), cap the total body bytes, and use a `VecDeque` plus an id offset instead of rebuilding the index.
- **Related:** `src/ui/inspector_view.rs:537-556` saves `network.snapshot()` (up to 800 entries) to session storage every 400 ms while recording. Save only when recording stops or the page unloads, or every few seconds with a smaller cap.

### G12. Compile rules once per batch
- **Where:**
  - `src/state.rs:92` `apply_auto_rules` calls `rule.pattern.compile()` per rule per entry.
  - `src/workspace.rs:316` `place_entry` builds a new `Placer` per entry.
  - Both run in loops in `import_urls` (`state.rs:553`), `import_sessions` (`:576`) and `apply_auto_rules_all` (`:108`).
  - `src/ui/tabs_view.rs:689` recompiles the rule every frame. `:647-648` and `:732` count matches with linear lookups every frame.
- **Fix:** compile once per batch (as `Placer` already allows; add the same for auto rules), or store compiled rules in a non-serialized field. In the UI, store the compiled rule and match count per `(rule_kind, rule_value, revision)`.

### G13. Remove unused favicon data
- **Where:** `src/model.rs:380` `BrowserTab.fav_icon_url` is never read in Rust. It is still filled by `tabOut` (often with a multi-KB `data:` URL) for every tab on every `query_tabs()`. `favIconUrl` in the JS `TRACKED` list also fires `tabUpdated` events.
- **Fix:** remove the field, and remove `favIconUrl` from `tabOut` and `TRACKED` in `web/platform-common.js`. First check that no UI code shows favicons from this data.

### G14. Skip `query_tabs()` in `sync_tab_strip` when nothing needs it
- **Where:** `src/background.rs:1269`. Also `chrome/platform.js:32-43,57-62`: `syncThrottle()` calls `tabs.query({})` on every tab update even when there are no throttle rules.
- **Fix:** return early when hiding, groups and title preface are all off (and, for throttle, when no rule is enabled and `tabUrls` is empty). Check only the tab that changed.

### G15. Ad analysis: speed up the URL scan and store the results
- **Where:** `src/ads.rs:579` `urls_in` runs `rest.find(..)` for 3 patterns on every iteration. A pattern that never appears rescans the rest of the text each time. `plain_code` and `urls_in` run twice per signal (`:515`, `:547`).
- **Fix:** make it a single forward pass: find `//`, then check whether `http:` or `https:` comes right before it. Compute it once per signal and share the result. The UI caching is covered by G3.
- **Done when:** add a test with a 500 KB script that checks the URLs found are unchanged.

### G16. Firefox debugger-stripping: process scripts as they arrive
- **Where:** `firefox/platform.js:17-35` holds every script response in memory (copied up to 3 times) until the last byte.
- **Fix:** strip each chunk and write it out right away, holding back only the last ~16 bytes for a match that spans two chunks. Pass chunks without the target bytes straight through.
- **Done when:** the output is byte-for-byte the same as now. Add a test where the match spans a chunk boundary.

### G17. Capture live-feed queue has no limit
- **Where:** `web/capture-engine.js:146-210`. If `addSourceBuffer` throws or the MediaSource closes, `pump()` returns early forever and `feed.push` keeps every chunk.
- **Fix:** when `!sb` or `ms.readyState === 'closed'`, drop new chunks (or set `feed = null`), and keep only the last few chunks in the queue.

### G18. Inspector MutationObserver allocations
- **Where:** `web/inspect.js:265-285` builds `[...r.addedNodes, ...r.removedNodes]` for every record. `listed` (`:258`) and `nodes` (`:22`) never shrink.
- **Fix:** run the cheap checks first, spread the node lists only for `childList` records, and remove dead `WeakRef` entries when flushing.

### G19. Don't parse state that a page never shows
- **Where:** `src/ui/mod.rs:408-411`. Every page (sidebar, inspector, capture window) parses `SNAPSHOTS_KEY` into a `BTreeMap` on every change, and the capture window parses the full `AppState`.
- **Fix:** register the snapshots listener only in the manager and popup, and skip state parsing in capture mode.

### G20. Don't clone stream manifests every frame
- **Where:** `src/ui/capture_window.rs:1177` `mpd.clone()` (every representation with all its segments). `:1141` clones the HLS master and re-sorts its variants.
- **Fix:** borrow `&self.stream`. Store the sorted variants.
- **Optional:** `src/stream.rs:764` `parse_mpd` builds every segment URL for every representation. Build them only for the chosen track.

---

## P3: low impact / cleanup

- **G21.**
  - `src/ui/tabs_view.rs:1185-1187` and `src/ui/sidebar_view.rs:268` build the `DraggedTab` title for every row every frame. Do it only inside `if resp.drag_started()`.
  - For long lists, build a flat row list and draw it with `show_rows`/`show_viewport`.
  - The per-frame `owner`/`listed` HashMaps at `tabs_view.rs:173-192` should be stored too.
- **G22.** `src/ui/backup_view.rs:164-167` rebuilds `BTreeSet` diffs per snapshot every frame. Store the counts per `(snapshot id, revision)`.
- **G23.** `src/ui/capture_view.rs:170` calls `request_repaint_after(1500ms)` with no condition. Poll only while captures exist or a capture window is open.
- **G24.** `src/dedupe.rs:158-161` runs `retain` once per removed ref. Collect a `HashSet` of ids and do one `retain` per session. `:200` builds `key()` for every entry on every navigation; keep a key→entry map.
- **G25.** `OPENED` (`src/background.rs:61`), `STREAM_RELOADED` and `TITLES` are never pruned. Remove the tab's entries on `TabRemoved`.
- **G26.** Smaller per-call costs:
  - `snapshots_fingerprint` (`src/background.rs:108`) clones snapshot names on every save.
  - `src/hibernate.rs:38` and `src/workspace.rs:297` call `contains` on a slice inside a loop; use a `HashSet`.
- **G27.** Guards (`web/guards/*.js`): up to six `Function.prototype.toString` Proxy layers, and `dispatchEvent` is wrapped two or three times (`popups.js:86`, `click-hijack.js:161`, `popups-strict.js:25`). Share one toString disguise, or bundle the enabled guards into one registered script. **Care:** this is sensitive to detection by pages; test with `tools/guards-test.html`.
- **G28.** `src/ui/mini_view.rs:62` sorts the popup list every frame. Store the sorted list per `(revision, query)`.

## Out of scope / checked and fine

- Blob URLs are revoked, and `text-selection.js` disconnects its observer.
- `webm.rs` works only on the head of the file. `worker.rs` and the single `Closure::forget` at startup are fine.
- The popup, cleanup and network logs all have caps.
- Base64 over ports in capture (`capture-page.js:189,336`) costs about 33% extra, but Chrome's messaging limits make it acceptable. Change it only if capture throughput becomes a problem.
