// Page guard: the page can't block selecting and copying text.
(() => {
  for (const type of ['selectstart', 'copy', 'cut']) {
    window.addEventListener(type, (e) => e.stopImmediatePropagation(), true);
  }
  // Undo user-select: none. The style goes into <head>, where pages keep their own styles
  // (at document_start there is no <head> yet).
  const style = document.createElement('style');
  style.textContent = '*, *::before, *::after { -webkit-user-select: text !important; user-select: text !important; }';
  const add = () => {
    if (!document.head) return false;
    document.head.appendChild(style);
    return true;
  };
  if (!add()) {
    const observer = new MutationObserver(() => add() && observer.disconnect());
    observer.observe(document, { childList: true, subtree: true });
  }
})();
