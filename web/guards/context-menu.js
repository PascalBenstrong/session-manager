// Page guard: the page can't block right-clicking. Its context menu and right-button
// handlers never see the events, so they can't cancel them, and the browser's menu opens.
(() => {
  const stop = (e) => e.stopImmediatePropagation();
  window.addEventListener('contextmenu', stop, true);
  for (const type of ['mousedown', 'mouseup', 'pointerdown', 'pointerup', 'auxclick']) {
    window.addEventListener(type, (e) => e.button === 2 && e.stopImmediatePropagation(), true);
  }
})();
