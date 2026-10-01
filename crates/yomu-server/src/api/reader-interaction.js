// Shared trusted iframe interaction bridge: embedded under the EPUB nonce,
// and packaged as a local lazy PDF asset. Pointer events alone never block
// native scrolling, selection, links or the format navigator's gestures.
(function () {
  window.YomuReaderInteraction = function (notify) {
    var tap = null, timer = 0;
    function selection() { var value = getSelection(); return value && !value.isCollapsed; }
    function interactive(target) {
      return target.closest && target.closest('a, button, input, select, textarea, [contenteditable]');
    }
    function cancel() { clearTimeout(timer); timer = 0; tap = null; }
    addEventListener('pointerdown', function (event) {
      cancel();
      if (!event.isPrimary || event.button !== 0 || event.ctrlKey || event.altKey || event.metaKey) return;
      tap = { x: event.clientX, y: event.clientY, at: performance.now(), moved: false, selected: selection(), pointer: event.pointerType };
    }, { passive: true });
    addEventListener('pointermove', function (event) {
      if (tap && Math.hypot(event.clientX - tap.x, event.clientY - tap.y) > 8) tap.moved = true;
    }, { passive: true });
    addEventListener('pointercancel', cancel);
    addEventListener('touchstart', function (event) { if (event.touches.length > 1) cancel(); }, { passive: true });
    addEventListener('touchcancel', cancel);
    addEventListener('contextmenu', cancel);
    addEventListener('click', function (event) {
      clearTimeout(timer);
      var candidate = tap;
      tap = null;
      if (!candidate || candidate.moved || candidate.selected || selection() || interactive(event.target)
          || (event.detail > 1 && candidate.pointer !== 'touch') || performance.now() - candidate.at > 600) return;
      // Give mouse double-click/word-selection its second pointerdown before
      // interpreting a single click as chrome intent. Touch click.detail can
      // also increment on two separate taps: selection, not that counter,
      // distinguishes word selection from an intentional hide/show pair.
      timer = setTimeout(function () {
        if (!selection()) notify('yomu-chrome:toggle');
      }, 240);
    });
    addEventListener('keydown', function (event) {
      if (event.key === 'Escape') { cancel(); notify('yomu-chrome:show'); }
    });
  };
}());
