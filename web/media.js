// Content script (every frame): reports media playback to the background, pauses /
// resumes media when the background asks (the background media limit, see src/media.rs),
// and stops / resumes streams: a button over playing videos asks the background to hold
// back the tab's media data, and the videos are paused meanwhile (optionally with a
// low-resolution loop of their last frames over them). It only talks to this extension.
(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api || !api.runtime || !api.runtime.id || globalThis.__smMedia) return;
  globalThis.__smMedia = true;

  const pausedByUs = new Set();
  let last = JSON.stringify({ playing: false, video: false, pip: false });

  const elements = () => document.querySelectorAll('video, audio');
  const isPlaying = (m) => !m.paused && !m.ended;
  const isVideo = (m) => m instanceof HTMLVideoElement;
  // Videos playing in this frame (kept from their events: nothing to look up otherwise).
  const playingVideos = new Set();
  document.addEventListener('playing', (e) => isVideo(e.target) && playingVideos.add(e.target), true);
  for (const type of ['pause', 'ended', 'emptied']) document.addEventListener(type, (e) => playingVideos.delete(e.target), true);

  function message(msg) {
    try {
      const p = api.runtime.sendMessage(msg);
      if (p && p.catch) p.catch(() => {});
    } catch {
      // The extension was reloaded; this script is orphaned.
    }
  }
  const send = (state) => message({ smMedia: state });
  const signal = (what) => message({ smStream: what });

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

  // ------------------------------------------------------------ stopped streams

  let config = { replay: false, replayHeight: 240, replayFrames: 30 };
  let stopped = false;
  let stoppedAt = 0;
  const stoppedByUs = new Set(); // media we paused when stopping, resumed with the stream
  // The user's last click or key press in the page (not on our button), to tell their
  // "play" from the player's own attempts.
  let lastInput = 0;
  for (const type of ['pointerdown', 'keydown']) {
    addEventListener(type, (e) => e.isTrusted && !(host && e.composedPath().includes(host)) && (lastInput = performance.now()), true);
  }

  // A new page in the tab: its player starts with the data flowing.
  if (window === window.top) signal('fresh');

  // Resuming: a live stream goes back to its live edge (players that don't do it
  // themselves stay far behind), and a player that doesn't recover gets its page reloaded.
  const LIVE_BEHIND = 15; // seconds behind the live edge that count as "far behind"
  function toLiveEdge(m) {
    try {
      const end = m.seekable.length ? m.seekable.end(m.seekable.length - 1) : NaN;
      if (m.duration === Infinity && end - m.currentTime > LIVE_BEHIND) m.currentTime = end - 3;
    } catch {}
  }
  function watch(m) {
    const from = m.currentTime;
    setTimeout(() => toLiveEdge(m), 2500);
    setTimeout(() => {
      if (stopped || !m.isConnected || (m.paused && !m.error)) return;
      if (m.error || m.currentTime - from < 1) signal('stuck');
    }, 15000);
  }

  function stopStreams() {
    stopped = true;
    stoppedAt = performance.now();
    for (const m of elements()) {
      if (!isPlaying(m)) continue;
      m.pause();
      stoppedByUs.add(m);
    }
    const shown = mainVideo([...stoppedByUs]);
    if (shown && config.replay) showReplay(shown);
    updateControl();
  }

  function resumeStreams() {
    stopped = false;
    hideReplay();
    const list = [...stoppedByUs].filter((m) => m.isConnected);
    stoppedByUs.clear();
    for (const m of list) {
      const p = m.play();
      if (p && p.catch) p.catch(() => {});
      watch(m);
    }
    updateControl();
  }

  // While stopped, the page (or its player) may try to play again: only the user can.
  document.addEventListener(
    'play',
    (e) => {
      if (!stopped || !(e.target instanceof HTMLMediaElement)) return;
      if (lastInput > stoppedAt && performance.now() - lastInput < 3000) {
        stoppedByUs.add(e.target);
        signal('toggle'); // the user pressed the player's own play button: resume
      } else {
        e.target.pause();
        stoppedByUs.add(e.target);
      }
    },
    true,
  );

  api.runtime.onMessage.addListener((msg) => {
    if (!msg) return;
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
    } else if (msg.smStreamCmd === 'stop') {
      stopStreams();
    } else if (msg.smStreamCmd === 'resume') {
      resumeStreams();
    }
  });

  // ---- the button over videos

  const MIN_W = 200;
  const MIN_H = 112;
  const rectOf = (el) => el.getBoundingClientRect();
  const bigEnough = (r) => r.width >= MIN_W && r.height >= MIN_H;
  const candidate = (v) => (isPlaying(v) || stoppedByUs.has(v)) && bigEnough(rectOf(v));
  // The biggest of these videos.
  const mainVideo = (list) =>
    list.filter(isVideo).sort((a, b) => rectOf(b).width * rectOf(b).height - rectOf(a).width * rectOf(a).height)[0] || null;

  let host = null;
  let button = null;
  let replayCanvas = null;
  let target = null; // the video the button is shown over
  let hideTimer = 0;

  function ensureHost() {
    if (host) return;
    host = document.createElement('sm-stream-control');
    const root = host.attachShadow({ mode: 'closed' });
    root.innerHTML = `<style>
      :host { all: initial; position: fixed; inset: 0 auto auto 0; width: 0; height: 0; z-index: 2147483647; pointer-events: none; }
      button { all: initial; position: fixed; pointer-events: auto; cursor: pointer; display: none; align-items: center; gap: 6px;
        font: 600 12px/1 system-ui, sans-serif; color: #fff; background: rgba(18, 18, 22, 0.8); border: 1px solid rgba(255, 255, 255, 0.28);
        border-radius: 14px; padding: 6px 11px; box-shadow: 0 2px 8px rgba(0, 0, 0, 0.35); }
      button:hover { background: rgba(48, 48, 58, 0.94); }
      button.on { display: flex; }
      canvas { position: fixed; display: none; pointer-events: none; background: #000; }
      canvas.on { display: block; }
    </style><canvas></canvas><button type="button"></button>`;
    button = root.querySelector('button');
    replayCanvas = root.querySelector('canvas');
    // The click is ours: the page under the button (the player) doesn't get it.
    for (const type of ['pointerdown', 'pointerup', 'mousedown', 'mouseup', 'click', 'dblclick', 'contextmenu']) {
      button.addEventListener(type, (e) => {
        e.stopPropagation();
        e.preventDefault();
        if (type === 'click') signal('toggle');
      });
    }
  }

  // Attached where it shows: in the element shown full screen, if any.
  function attach() {
    const parent = document.fullscreenElement && !(document.fullscreenElement instanceof HTMLMediaElement) ? document.fullscreenElement : document.documentElement;
    if (host.parentNode !== parent) parent.appendChild(host);
  }

  function place() {
    if (!target || !target.isConnected) return;
    const r = rectOf(target);
    button.style.left = `${Math.round(r.left + 10)}px`;
    button.style.top = `${Math.round(r.top + 10)}px`;
    if (replayCanvas.classList.contains('on')) {
      Object.assign(replayCanvas.style, { left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
    }
  }

  // A stopped video keeps its button; otherwise it shows while the pointer is over it.
  function updateControl() {
    const v = stopped && mainVideo([...stoppedByUs]);
    if (v) return showOver(v, false);
    if (!button) return;
    updateLabel();
    clearTimeout(hideTimer);
    hideTimer = setTimeout(hide, 2500);
  }

  function showOver(v, autoHide) {
    try {
      ensureHost();
      attach();
      target = v;
      button.classList.add('on');
      updateLabel();
      place();
      clearTimeout(hideTimer);
      if (autoHide) hideTimer = setTimeout(hide, 2500);
    } catch {}
  }
  function updateLabel() {
    button.textContent = stopped ? '▶  Resume stream' : '⏸  Stop stream';
    button.title = stopped
      ? 'Resume the stream (its data is held back meanwhile)'
      : 'Stop the stream: pause it and hold back its data until you resume it (Session Manager)';
  }
  function hide() {
    if (stopped && target && stoppedByUs.has(target)) return;
    button && button.classList.remove('on');
    target = null;
  }

  let pointer = null;
  let frame = 0;
  addEventListener(
    'pointermove',
    (e) => {
      // Nothing to do in pages (and frames) without a playing or stopped video.
      if (!playingVideos.size && !stoppedByUs.size) return;
      pointer = [e.clientX, e.clientY];
      if (!frame) frame = requestAnimationFrame(hoverCheck);
    },
    { capture: true, passive: true },
  );
  function hoverCheck() {
    frame = 0;
    if (!pointer) return;
    const [x, y] = pointer;
    for (const v of new Set([...playingVideos, ...stoppedByUs])) {
      if (!isVideo(v) || !v.isConnected || !candidate(v)) continue;
      const r = rectOf(v);
      if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) return showOver(v, !stopped);
    }
  }
  for (const type of ['scroll', 'resize']) addEventListener(type, () => target && place(), { capture: true, passive: true });
  document.addEventListener('fullscreenchange', () => {
    if (!host || !target) return;
    attach();
    place();
  });

  // ---- replay: a low-resolution loop of the last frames while stopped

  const rings = new WeakMap(); // video -> { frames: [{ canvas, t }], next }
  let capturing = new Set();

  function applyConfig(c) {
    const was = config.replay;
    config = { ...config, ...c };
    if (!config.replay || !was) {
      // Off: forget the frames. On: start with the videos playing now.
      for (const v of capturing) rings.delete(v);
      capturing = new Set();
    }
    if (config.replay) for (const v of document.querySelectorAll('video')) if (isPlaying(v)) capture(v);
  }
  // A video that's done (emptied) lets its frames go.
  document.addEventListener('emptied', (e) => capturing.delete(e.target) && rings.delete(e.target), true);
  // Copies: at most 10 a second, of the biggest playing video of the frame, while shown.
  const REPLAY_GAP_MS = 100;
  try {
    const got = api.storage && api.storage.local.get('smStreamConfig');
    if (got && got.then) got.then((r) => r && r.smStreamConfig && applyConfig(r.smStreamConfig)).catch(() => {});
    api.storage && api.storage.onChanged.addListener((changes, area) => {
      if (area === 'local' && changes.smStreamConfig) applyConfig(changes.smStreamConfig.newValue || {});
    });
  } catch {}
  document.addEventListener('playing', (e) => config.replay && isVideo(e.target) && capture(e.target), true);

  function capture(v) {
    if (capturing.has(v) || !v.requestVideoFrameCallback) return;
    capturing.add(v);
    const ring = { frames: [], next: 0, last: 0 };
    rings.set(v, ring);
    const grab = (now) => {
      if (!capturing.has(v) || !config.replay) return capturing.delete(v);
      try {
        const due = now - ring.last >= REPLAY_GAP_MS && !document.hidden;
        if (due && v.videoWidth && v.videoHeight && bigEnough(rectOf(v)) && (playingVideos.size < 2 || mainVideo([...playingVideos]) === v)) {
          ring.last = now;
          const h = Math.min(config.replayHeight, v.videoHeight);
          const w = Math.max(1, Math.round((v.videoWidth * h) / v.videoHeight));
          const n = config.replayFrames;
          if (ring.frames.length > n) ring.frames.length = n;
          let slot = ring.frames.length < n ? null : ring.frames[ring.next % n];
          if (!slot) {
            slot = { canvas: document.createElement('canvas'), t: 0 };
            ring.frames.push(slot);
          }
          if (slot.canvas.width !== w || slot.canvas.height !== h) Object.assign(slot.canvas, { width: w, height: h });
          slot.canvas.getContext('2d', { alpha: false }).drawImage(v, 0, 0, w, h);
          slot.t = now;
          ring.next = (ring.next + 1) % n;
        }
      } catch {}
      v.requestVideoFrameCallback(grab);
    };
    v.requestVideoFrameCallback(grab);
  }

  let replayTimer = 0;
  function showReplay(v) {
    const ring = rings.get(v);
    if (!ring || !ring.frames.length) return;
    // Oldest first.
    const n = ring.frames.length;
    const frames = ring.frames.length < config.replayFrames ? ring.frames.slice() : [...ring.frames.slice(ring.next), ...ring.frames.slice(0, ring.next)];
    const gaps = frames.slice(1).map((f, i) => f.t - frames[i].t).filter((d) => d > 0);
    const step = Math.min(200, Math.max(16, gaps.length ? gaps.reduce((a, b) => a + b, 0) / gaps.length : 40));
    try {
      ensureHost();
      attach();
      target = v;
      replayCanvas.classList.add('on');
      place();
      const r = rectOf(v);
      replayCanvas.width = Math.max(1, Math.round(r.width));
      replayCanvas.height = Math.max(1, Math.round(r.height));
      const ctx = replayCanvas.getContext('2d', { alpha: false });
      let i = 0;
      const draw = () => {
        if (!replayCanvas.classList.contains('on')) return;
        const f = frames[i % n].canvas;
        // Fit inside, like the video does.
        const scale = Math.min(replayCanvas.width / f.width, replayCanvas.height / f.height);
        const w = f.width * scale;
        const h = f.height * scale;
        ctx.fillStyle = '#000';
        ctx.fillRect(0, 0, replayCanvas.width, replayCanvas.height);
        ctx.drawImage(f, (replayCanvas.width - w) / 2, (replayCanvas.height - h) / 2, w, h);
        i++;
        replayTimer = setTimeout(draw, document.hidden ? 1000 : step);
      };
      draw();
    } catch {}
  }
  function hideReplay() {
    clearTimeout(replayTimer);
    if (replayCanvas) replayCanvas.classList.remove('on');
  }
})();
