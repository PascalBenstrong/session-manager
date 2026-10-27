// A steady timer for the capture window (web/capture-engine.js): timers in workers keep
// their pace when the window is in the background, where the window's own are slowed down.
let timer = 0;
onmessage = (e) => {
  clearInterval(timer);
  timer = setInterval(() => postMessage(0), Number(e.data) || 33);
};
