//! Background media limit: only the active tab (or a limited number of background tabs)
//! may play media. A content script (`web/media.js`) reports playback in each frame and
//! pauses / resumes it when asked; this module decides which tabs to pause.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{BrowserTab, TabId};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MediaPolicy {
    /// Media plays everywhere.
    #[default]
    Off,
    /// Only the active tab of each window plays media.
    ActiveTabOnly,
    /// Besides the active tabs, at most `max_background` tabs play media.
    LimitBackground,
}

impl MediaPolicy {
    pub const ALL: [MediaPolicy; 3] = [MediaPolicy::Off, MediaPolicy::ActiveTabOnly, MediaPolicy::LimitBackground];

    pub fn label(self) -> &'static str {
        match self {
            MediaPolicy::Off => "No limit",
            MediaPolicy::ActiveTabOnly => "Only in the active tab",
            MediaPolicy::LimitBackground => "Limit background tabs",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct MediaSettings {
    pub policy: MediaPolicy,
    /// Background tabs allowed to play with [`MediaPolicy::LimitBackground`].
    pub max_background: u32,
    /// Also limit audio-only playback (music, podcasts). Otherwise only video counts.
    pub include_audio: bool,
    /// Resume media we paused when its tab becomes active again.
    pub resume_on_activate: bool,
    /// A video in picture-in-picture counts as visible and keeps playing.
    pub allow_pip: bool,
    /// While a stream is stopped, loop a low-resolution copy of its last frames over the
    /// paused video (kept in the page's memory, no data).
    pub stop_replay: bool,
    /// Height of the replay copy, in pixels.
    pub replay_height: u32,
    /// How many of the last frames are replayed.
    pub replay_frames: u32,
}

impl Default for MediaSettings {
    fn default() -> Self {
        Self {
            policy: MediaPolicy::Off,
            max_background: 1,
            include_audio: false,
            resume_on_activate: true,
            allow_pip: true,
            stop_replay: false,
            replay_height: 240,
            replay_frames: 30,
        }
    }
}

/// Choices for [`MediaSettings::replay_height`] and the range of `replay_frames`.
pub const REPLAY_HEIGHTS: [u32; 4] = [96, 144, 240, 360];
pub const REPLAY_FRAMES: std::ops::RangeInclusive<u32> = 1..=120;

impl MediaSettings {
    /// What the media content script needs to know (stored for it, see `web/media.js`).
    pub fn stream_config(&self) -> serde_json::Value {
        serde_json::json!({
            "replay": self.stop_replay,
            "replayHeight": self.replay_height.clamp(48, 720),
            "replayFrames": self.replay_frames.clamp(*REPLAY_FRAMES.start(), *REPLAY_FRAMES.end()),
        })
    }

    pub fn enabled(&self) -> bool {
        self.policy != MediaPolicy::Off
    }

    /// How many background tabs may play.
    fn allowed_background(&self) -> usize {
        match self.policy {
            MediaPolicy::Off => usize::MAX,
            MediaPolicy::ActiveTabOnly => 0,
            MediaPolicy::LimitBackground => self.max_background as usize,
        }
    }
}

/// Media playing in one frame of a tab, as reported by the content script.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Playback {
    pub tab_id: TabId,
    pub frame_id: i64,
    /// A video is playing (otherwise audio only).
    pub video: bool,
    /// The video is shown picture-in-picture.
    pub pip: bool,
    /// When the playback started (ms since epoch).
    pub since: f64,
}

/// What the content script reports for a frame.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(default)]
pub struct MediaReport {
    pub playing: bool,
    pub video: bool,
    pub pip: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct MediaState {
    pub playing: Vec<Playback>,
    /// Tabs whose media we paused (resumed when they become active).
    pub paused: BTreeSet<TabId>,
    /// Tabs whose streams the user stopped: paused, and their media data held back until
    /// they are resumed.
    pub stopped: BTreeSet<TabId>,
}

/// Tabs to pause / resume.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaPlan {
    pub pause: Vec<TabId>,
    pub resume: Vec<TabId>,
}

impl MediaPlan {
    pub fn is_empty(&self) -> bool {
        self.pause.is_empty() && self.resume.is_empty()
    }
}

impl MediaState {
    /// Records a frame's report. Returns true if anything changed.
    pub fn report(&mut self, tab_id: TabId, frame_id: i64, r: MediaReport, now: f64) -> bool {
        let pos = self.playing.iter().position(|p| p.tab_id == tab_id && p.frame_id == frame_id);
        let mut changed = false;
        if r.playing {
            // The user (or the page) started playing again: it is no longer ours to resume.
            changed |= self.paused.remove(&tab_id);
        }
        match (pos, r.playing) {
            (Some(i), true) => {
                let p = &mut self.playing[i];
                changed |= p.video != r.video || p.pip != r.pip;
                p.video = r.video;
                p.pip = r.pip;
            }
            (None, true) => {
                self.playing.push(Playback { tab_id, frame_id, video: r.video, pip: r.pip, since: now });
                changed = true;
            }
            (Some(i), false) => {
                self.playing.remove(i);
                changed = true;
            }
            (None, false) => {}
        }
        changed
    }

    pub fn forget_tab(&mut self, tab_id: TabId) -> bool {
        let before = self.playing.len();
        self.playing.retain(|p| p.tab_id != tab_id);
        self.paused.remove(&tab_id) | self.stopped.remove(&tab_id) | (self.playing.len() != before)
    }

    /// Stops (`true`) or resumes a tab's streams. Returns true if that changed anything.
    pub fn set_stopped(&mut self, tab_id: TabId, stop: bool) -> bool {
        if stop {
            // Ours now, not the media limit's to resume.
            self.paused.remove(&tab_id);
            self.stopped.insert(tab_id)
        } else {
            self.stopped.remove(&tab_id)
        }
    }

    /// Tabs playing (or stopped) that aren't the active tab of their window: what "stop
    /// background streams" stops.
    pub fn background_streams(&self, tabs: &[BrowserTab]) -> Vec<TabId> {
        let active: BTreeSet<TabId> = tabs.iter().filter(|t| t.active).map(|t| t.id).collect();
        let tabs: BTreeSet<TabId> = self.playing.iter().map(|p| p.tab_id).filter(|t| !active.contains(t) && !self.stopped.contains(t)).collect();
        tabs.into_iter().collect()
    }

    pub fn replace_tab(&mut self, added: TabId, removed: TabId) {
        for p in self.playing.iter_mut().filter(|p| p.tab_id == removed) {
            p.tab_id = added;
        }
        if self.paused.remove(&removed) {
            self.paused.insert(added);
        }
        if self.stopped.remove(&removed) {
            self.stopped.insert(added);
        }
    }

    /// Drops tabs that are no longer open.
    pub fn retain_open(&mut self, open: &BTreeSet<TabId>) {
        self.playing.retain(|p| open.contains(&p.tab_id));
        self.paused.retain(|t| open.contains(t));
        self.stopped.retain(|t| open.contains(t));
    }

    pub fn is_empty(&self) -> bool {
        self.playing.is_empty() && self.paused.is_empty() && self.stopped.is_empty()
    }

    /// Tabs playing media that counts for `settings`.
    pub fn playing_tabs(&self, settings: &MediaSettings) -> BTreeSet<TabId> {
        self.playing.iter().filter(|p| p.video || settings.include_audio).map(|p| p.tab_id).collect()
    }

    /// Decides which tabs to pause and resume, given the open tabs.
    ///
    /// Active tabs (and picture-in-picture videos, if allowed) always play. Of the other
    /// playing tabs, the most recently used ones are kept up to the limit.
    pub fn plan(&self, settings: &MediaSettings, tabs: &[BrowserTab]) -> MediaPlan {
        let mut plan = MediaPlan::default();
        if !settings.enabled() {
            return plan;
        }
        let by_id: BTreeMap<TabId, &BrowserTab> = tabs.iter().map(|t| (t.id, t)).collect();
        let is_active = |id: &TabId| by_id.get(id).is_some_and(|t| t.active);

        // tab -> (picture-in-picture, playback start)
        let mut playing: BTreeMap<TabId, (bool, f64)> = BTreeMap::new();
        for p in self.playing.iter().filter(|p| p.video || settings.include_audio) {
            let e = playing.entry(p.tab_id).or_insert((false, p.since));
            e.0 |= p.pip;
            e.1 = e.1.min(p.since);
        }
        let mut background: Vec<(TabId, f64)> = playing
            .iter()
            .filter(|(id, (pip, _))| !is_active(id) && !(settings.allow_pip && *pip))
            .map(|(id, (_, since))| {
                let used = by_id.get(id).map_or(0.0, |t| t.last_accessed);
                (*id, if used > 0.0 { used } else { *since })
            })
            .collect();
        // Most recently used first; keep those up to the limit.
        background.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        plan.pause = background.into_iter().skip(settings.allowed_background()).map(|(id, _)| id).collect();
        plan.pause.retain(|t| !self.stopped.contains(t));
        if settings.resume_on_activate {
            plan.resume = self.paused.iter().copied().filter(|t| is_active(t) && !self.stopped.contains(t)).collect();
        }
        plan
    }

    /// Records that the plan was carried out.
    pub fn apply(&mut self, plan: &MediaPlan) {
        for id in &plan.pause {
            self.playing.retain(|p| p.tab_id != *id);
            self.paused.insert(*id);
        }
        for id in &plan.resume {
            self.paused.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: TabId, active: bool, last_accessed: f64) -> BrowserTab {
        BrowserTab { id, window_id: 1, active, last_accessed, ..Default::default() }
    }

    const VIDEO: MediaReport = MediaReport { playing: true, video: true, pip: false };

    fn settings(policy: MediaPolicy, max: u32) -> MediaSettings {
        MediaSettings { policy, max_background: max, ..Default::default() }
    }

    #[test]
    fn stopped_streams_stay_stopped() {
        let mut m = MediaState::default();
        m.report(1, 0, VIDEO, 1.0);
        m.report(1, 3, VIDEO, 1.0);
        m.report(2, 0, VIDEO, 2.0);
        m.report(3, 0, VIDEO, 3.0);
        let tabs = [tab(1, false, 5.0), tab(2, true, 6.0), tab(3, false, 4.0)];
        assert_eq!(m.background_streams(&tabs), vec![1, 3], "one entry per tab, not the active one");
        // The media limit paused tab 1; the user then stops it.
        m.paused.insert(1);
        assert!(m.set_stopped(1, true));
        assert!(!m.set_stopped(1, true));
        assert!(!m.paused.contains(&1));
        assert_eq!(m.background_streams(&tabs), vec![3]);
        // Activating it doesn't resume it, and the limit doesn't pause it again.
        let tabs = [tab(1, true, 9.0), tab(2, false, 6.0), tab(3, false, 4.0)];
        let plan = m.plan(&settings(MediaPolicy::ActiveTabOnly, 0), &tabs);
        assert!(!plan.resume.contains(&1) && !plan.pause.contains(&1));
        m.replace_tab(11, 1);
        assert!(m.stopped.contains(&11));
        assert!(m.set_stopped(11, false));
        assert!(m.stopped.is_empty());
    }

    #[test]
    fn active_tab_only() {
        let mut m = MediaState::default();
        m.report(1, 0, VIDEO, 1.0);
        m.report(2, 0, VIDEO, 2.0);
        let tabs = [tab(1, false, 5.0), tab(2, true, 6.0)];
        let plan = m.plan(&settings(MediaPolicy::ActiveTabOnly, 0), &tabs);
        assert_eq!(plan.pause, vec![1]);
        assert!(m.plan(&settings(MediaPolicy::Off, 0), &tabs).is_empty());
        m.apply(&plan);
        assert!(m.paused.contains(&1));
        assert_eq!(m.playing.len(), 1);

        // switching back to tab 1 resumes it; tab 2 is now in the background
        m.report(2, 0, VIDEO, 2.0);
        let tabs = [tab(1, true, 9.0), tab(2, false, 6.0)];
        let plan = m.plan(&settings(MediaPolicy::ActiveTabOnly, 0), &tabs);
        assert_eq!(plan, MediaPlan { pause: vec![2], resume: vec![1] });
    }

    #[test]
    fn limit_keeps_most_recently_used() {
        let mut m = MediaState::default();
        for (id, at) in [(1, 1.0), (2, 2.0), (3, 3.0), (4, 4.0)] {
            m.report(id, 0, VIDEO, at);
        }
        // tab 4 is active; 2 was used last of the others, then 3, then 1
        let tabs = [tab(1, false, 10.0), tab(2, false, 30.0), tab(3, false, 20.0), tab(4, true, 40.0)];
        let mut plan = m.plan(&settings(MediaPolicy::LimitBackground, 2), &tabs).pause;
        plan.sort();
        assert_eq!(plan, vec![1]);
        assert_eq!(m.plan(&settings(MediaPolicy::LimitBackground, 5), &tabs).pause, Vec::<TabId>::new());
    }

    #[test]
    fn audio_pip_and_frames() {
        let mut m = MediaState::default();
        m.report(1, 0, MediaReport { playing: true, video: false, pip: false }, 1.0);
        m.report(2, 3, MediaReport { playing: true, video: true, pip: true }, 1.0);
        let tabs = [tab(1, false, 1.0), tab(2, false, 1.0), tab(3, true, 1.0)];
        let s = settings(MediaPolicy::ActiveTabOnly, 0);
        assert!(m.plan(&s, &tabs).pause.is_empty(), "audio is not limited, pip keeps playing");
        let strict = MediaSettings { include_audio: true, allow_pip: false, ..s };
        assert_eq!(m.plan(&strict, &tabs).pause, vec![1, 2]);
        // a frame stopping removes only that frame
        assert!(m.report(2, 3, MediaReport::default(), 2.0));
        assert!(!m.report(2, 3, MediaReport::default(), 2.0));
        assert_eq!(m.playing.len(), 1);
    }

    #[test]
    fn user_resuming_clears_paused() {
        let mut m = MediaState::default();
        m.report(1, 0, VIDEO, 1.0);
        m.apply(&MediaPlan { pause: vec![1], resume: vec![] });
        m.report(1, 0, VIDEO, 2.0);
        assert!(m.paused.is_empty());
        m.replace_tab(7, 1);
        assert_eq!(m.playing[0].tab_id, 7);
        assert!(m.forget_tab(7));
        assert!(m.is_empty());
    }
}
