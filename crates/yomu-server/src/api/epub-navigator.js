// Trusted, nonce-authorized bridge in an otherwise script-disabled EPUB.
(function () {
  var root = document.documentElement;
  var theme = new URLSearchParams(location.search).get('yomu-theme');
  root.dataset.yomuTheme = theme === 'night' ? 'night' : 'paper';
  var widths = { narrow: '36rem', comfortable: '48rem', wide: '64rem', full: 'none' };
  var initialWidth = new URLSearchParams(location.search).get('yomu-width');
  root.style.setProperty('--reader-width', Object.hasOwn(widths, initialWidth) ? widths[initialWidth] : widths.comfortable);
  window.YomuReaderInteraction(function (message) { parent.postMessage(message, '*'); });
  var timer = 0;
  function progression() {
    var scroller = document.scrollingElement || root;
    var max = Math.max(0, scroller.scrollHeight - innerHeight);
    return max ? Math.max(0, Math.min(1000, Math.round(scroller.scrollTop / max * 1000))) : 0;
  }
  function send() { parent.postMessage('yomu-location:' + progression(), '*'); }
  function turn(delta) { parent.postMessage('yomu-epub-turn:' + delta, '*'); }
  addEventListener('scroll', function () {
    clearTimeout(timer); timer = setTimeout(send, 120);
  }, { passive: true });
  addEventListener('click', function (event) {
    var anchor = event.target.closest && event.target.closest('a[href]');
    if (!anchor) return;
    var target = new URL(anchor.getAttribute('href'), location.href);
    if (target.origin === location.origin && target.pathname === location.pathname && target.hash) return;
    event.preventDefault();
    parent.postMessage('yomu-link:' + target.href, '*');
  });
  var fontSize = 100;
  var layoutGeneration = 0;
  function reflow(change, at) {
    if (!document.body) return;
    if (at === undefined) at = progression();
    var generation = ++layoutGeneration;
    change();
    requestAnimationFrame(function () {
      if (generation !== layoutGeneration) return;
      var scroller = document.scrollingElement || root;
      scroller.scrollTop = Math.max(0, scroller.scrollHeight - innerHeight) * at / 1000;
      send();
    });
  }
  function setFontSize(value, at) {
    reflow(function () {
      fontSize = Math.max(70, Math.min(200, value));
      document.body.style.setProperty('font-size', fontSize + '%', 'important');
    }, at);
  }
  addEventListener('message', function (event) {
    if (event.source !== parent || typeof event.data !== 'string') return;
    if (event.data === 'yomu-epub-font:1') setFontSize(fontSize + 10);
    else if (event.data === 'yomu-epub-font:-1') setFontSize(fontSize - 10);
    else if (event.data === 'yomu-epub-theme:night') root.dataset.yomuTheme = 'night';
    else if (event.data === 'yomu-epub-theme:paper') root.dataset.yomuTheme = 'paper';
    else if (event.data.startsWith('yomu-epub-width:')) {
      var width = event.data.slice('yomu-epub-width:'.length);
      if (Object.hasOwn(widths, width)) reflow(function () { root.style.setProperty('--reader-width', widths[width]); });
    }
  });
  function interactive(target) {
    return target.closest && target.closest('a, button, input, select, textarea, [contenteditable]');
  }
  addEventListener('keydown', function (event) {
    if (event.ctrlKey || event.altKey || event.metaKey || interactive(event.target)) return;
    if (event.key === 'ArrowRight') { event.preventDefault(); turn(1); }
    if (event.key === 'ArrowLeft') { event.preventDefault(); turn(-1); }
  });
  addEventListener('wheel', function (event) {
    if (!event.ctrlKey) return;
    event.preventDefault();
    setFontSize(fontSize * Math.exp(-event.deltaY * 0.002));
  }, { passive: false });
  // Deliberate horizontal swipes change sections; vertical scrolling stays
  // native. A multi-touch gesture is never subsequently interpreted as a turn.
  var swipe, pinch, blocked = false;
  function distance(touches) { return Math.hypot(touches[0].clientX - touches[1].clientX, touches[0].clientY - touches[1].clientY); }
  addEventListener('touchstart', function (event) {
    if (event.touches.length === 2) {
      swipe = null; blocked = true;
      pinch = { distance: distance(event.touches), size: fontSize, at: progression() };
      event.preventDefault();
    } else if (event.touches.length === 1 && !blocked) {
      var selection = getSelection();
      if (interactive(event.target) || (selection && !selection.isCollapsed)) { swipe = null; return; }
      var touch = event.touches[0];
      swipe = { x: touch.clientX, y: touch.clientY, at: performance.now(), dx: 0, dy: 0 };
    } else { swipe = null; }
  }, { passive: false });
  addEventListener('touchmove', function (event) {
    if (pinch && event.touches.length === 2) {
      event.preventDefault();
      if (pinch.distance > 0) setFontSize(pinch.size * distance(event.touches) / pinch.distance, pinch.at);
    } else if (swipe && event.touches.length === 1) {
      swipe.dx = event.touches[0].clientX - swipe.x;
      swipe.dy = event.touches[0].clientY - swipe.y;
      if (Math.abs(swipe.dx) > 16 && Math.abs(swipe.dx) > Math.abs(swipe.dy) * 2) event.preventDefault();
    }
  }, { passive: false });
  addEventListener('touchend', function (event) {
    if (!blocked && swipe && event.touches.length === 0 && performance.now() - swipe.at < 700
        && Math.abs(swipe.dx) > 64 && Math.abs(swipe.dx) > Math.abs(swipe.dy) * 2) {
      turn(swipe.dx < 0 ? 1 : -1);
    }
    swipe = null; pinch = null;
    if (event.touches.length === 0) blocked = false;
  });
  addEventListener('touchcancel', function () { swipe = null; pinch = null; blocked = false; });
  addEventListener('load', function () {
    var match = location.hash.match(/^#yomu=(\d+)$/);
    if (match) {
      requestAnimationFrame(function () {
        var scroller = document.scrollingElement || root;
        scroller.scrollTop = Math.max(0, scroller.scrollHeight - innerHeight) * Math.min(1000, Number(match[1])) / 1000;
        send();
      });
    } else { send(); }
  });
}());
