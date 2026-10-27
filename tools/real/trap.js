// A developer tools trap written in a script file (only the Firefox script filter removes it).
window.__trapRuns = 0;
setInterval(function () {
  const t = performance.now();
  debugger;
  window.__trapRuns++;
  if (performance.now() - t > 100) window.report && window.report.detections.push('static debugger');
}, 300);
