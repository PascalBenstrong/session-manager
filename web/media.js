// Content script (every frame): reports media playback to the background, and pauses /
// resumes media when the background asks (the background media limit, see src/media.rs).
// It does nothing else, and only talks to this extension.
(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api || !api.runtime || !api.runtime.id || globalThis.__smMedia) return;
  globalThis.__smMedia = true;

  const pausedByUs = new Set();
  let last = JSON.stringify({ playing: false, video: false, pip: false });

  const elements = () => document.querySelectorAll('video, audio');
  const isPlaying = (m) => !m.paused && !m.ended;
  const isVideo = (m) => m instanceof HTMLVideoElement;

  function send(state) {
    try {
      const p = api.runtime.sendMessage({ smMedia: state });
      if (p && p.catch) p.catch(() => {});
    } catch {
      // The extension was reloaded; this script is orphaned.
    }
  }

  function report() {
    const playing = [...elements()].filter(isPlaying);
    const state = { playing: playing.length > 0, video: playing.some(isVideo), pip: !!document.pictureInPictureElement };
    const key = JSON.stringify(state);
    if (key === last) return;
    last = key;
    send(state);
  }

  let timer = 0;
  const schedule = () => {
    clearTimeout(timer);
    timer = setTimeout(report, 100);
  };
  // Media events don't bubble, but capturing listeners on the document see them all.
  for (const type of ['play', 'playing', 'pause', 'ended', 'emptied', 'enterpictureinpicture', 'leavepictureinpicture']) {
    document.addEventListener(type, schedule, true);
  }
  // Playing again after we paused it: the user (or page) wants it, it is no longer ours.
  document.addEventListener('play', (e) => pausedByUs.delete(e.target), true);
  addEventListener('pagehide', () => {
    if (JSON.parse(last).playing) {
      last = JSON.stringify({ playing: false, video: false, pip: false });
      send(JSON.parse(last));
    }
  });

  api.runtime.onMessage.addListener((msg) => {
    if (!msg || !msg.smMediaCmd) return;
    if (msg.smMediaCmd === 'pause') {
      for (const m of elements()) {
        if (isPlaying(m) && (msg.audio || isVideo(m)) && m !== document.pictureInPictureElement) {
          m.pause();
          pausedByUs.add(m);
        }
      }
    } else if (msg.smMediaCmd === 'resume') {
      for (const m of pausedByUs) {
        if (m.isConnected && m.paused) {
          const p = m.play();
          if (p && p.catch) p.catch(() => {});
        }
      }
      pausedByUs.clear();
    }
  });
})();
