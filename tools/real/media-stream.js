// A live video (a counter drawn on a canvas) with a sound track, for the media test pages.
function liveStream(color) {
  const c = document.createElement('canvas');
  c.width = 320;
  c.height = 180;
  const g = c.getContext('2d');
  let n = 0;
  setInterval(() => {
    g.fillStyle = color;
    g.fillRect(0, 0, 320, 180);
    g.fillStyle = '#fff';
    g.font = '64px sans-serif';
    g.fillText(String(n++), 20, 120);
  }, 33);
  const tracks = c.captureStream(30).getVideoTracks();
  try {
    const ac = new AudioContext();
    const osc = ac.createOscillator();
    const out = ac.createMediaStreamDestination();
    osc.connect(out);
    osc.start();
    tracks.push(...out.stream.getAudioTracks());
  } catch {}
  return new MediaStream(tracks);
}
