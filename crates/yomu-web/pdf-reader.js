import { getDocument, GlobalWorkerOptions, TextLayer } from './reader-assets/pdfjs/pdf.mjs';
import './reader-assets/reader-interaction.js';

GlobalWorkerOptions.workerSrc = new URL('./reader-assets/pdfjs/pdf.worker.mjs', import.meta.url).href;
const params = new URLSearchParams(location.search);
const status = document.querySelector('#status');
const container = document.querySelector('#page');
const canvas = container.querySelector('canvas');
const textContainer = container.querySelector('.textLayer');
const CSS_UNITS = 96 / 72;
const MAX_PIXELS = 16_000_000;
let textLayer, pdf, task;
let page = Math.max(0, Number(params.get('page')) || 0);
let generation = 0;
let mode = 'fit-width';
let scale = 1;
let renderedPage = -1;

function notify(message) { parent.postMessage(message, '*'); }
window.YomuReaderInteraction(notify);
function captureAnchor(x = innerWidth / 2, y = innerHeight / 2) {
  const rect = container.getBoundingClientRect();
  return { x, y, u: (x - rect.left) / rect.width, v: (y - rect.top) / rect.height };
}
function restoreAnchor(anchor) {
  if (!anchor || !Number.isFinite(anchor.u) || !Number.isFinite(anchor.v)) return;
  scrollTo(container.offsetLeft + anchor.u * container.clientWidth - anchor.x,
    container.offsetTop + anchor.v * container.clientHeight - anchor.y);
}
async function render(anchor) {
  if (!pdf) return;
  const current = ++generation;
  if (textLayer) textLayer.cancel();
  textContainer.replaceChildren();
  if (task) {
    task.cancel();
    try { await task.promise; } catch (_) { /* cancelled render */ }
  }
  try {
    const documentPage = await pdf.getPage(page + 1);
    if (current !== generation) return;
    const unit = documentPage.getViewport({ scale: CSS_UNITS });
    const width = Math.max(100, document.documentElement.clientWidth - 32);
    const padding = getComputedStyle(document.body);
    const height = Math.max(100, document.documentElement.clientHeight
      - parseFloat(padding.paddingTop) - parseFloat(padding.paddingBottom));
    if (mode === 'fit-width') scale = Math.min(4, width / unit.width);
    if (mode === 'fit-page') scale = Math.min(4, width / unit.width, height / unit.height);
    const viewport = documentPage.getViewport({ scale: scale * CSS_UNITS });
    // Keep large/zoomed pages from allocating unbounded high-DPI canvases.
    const ratio = Math.min(2, devicePixelRatio || 1, Math.sqrt(MAX_PIXELS / (viewport.width * viewport.height)));
    canvas.width = Math.ceil(viewport.width * ratio);
    canvas.height = Math.ceil(viewport.height * ratio);
    container.style.width = `${viewport.width}px`;
    container.style.height = `${viewport.height}px`;
    container.style.setProperty('--total-scale-factor', viewport.scale);
    document.documentElement.classList.toggle('zoomed', viewport.width > width + 1);
    if (renderedPage !== page) scrollTo(0, 0);
    else restoreAnchor(anchor);
    task = documentPage.render({
      canvasContext: canvas.getContext('2d'), viewport,
      transform: ratio === 1 ? undefined : [ratio, 0, 0, ratio, 0, 0],
    });
    await task.promise;
    if (current !== generation) return;
    textLayer = new TextLayer({ textContentSource: documentPage.streamTextContent(), container: textContainer, viewport });
    await textLayer.render();
    if (current !== generation) return;
    status.hidden = true;
    renderedPage = page;
    canvas.setAttribute('aria-label', `PDF page ${page + 1} of ${pdf.numPages}`);
    canvas.dataset.zoom = String(scale);
    canvas.dataset.zoomMode = mode;
    // Host progress and zoom are based on the actual rendered viewport.
    notify(`yomu-pdf-location:${page}:${pdf.numPages}`);
    notify(`yomu-pdf-zoom:${mode}:${scale}`);
  } catch (error) {
    if (current !== generation || error.name === 'RenderingCancelledException') return;
    status.hidden = false;
    status.textContent = `Could not render PDF: ${error.message}`;
    notify(`yomu-reader-error:${status.textContent}`);
  }
}
function setZoom(value, anchor = captureAnchor()) {
  if (!pdf) return;
  if (value === 'fit-width' || value === 'fit-page') mode = value;
  else {
    const target = value === 'in' ? scale * 1.25 : value === 'out' ? scale / 1.25 : Number(value);
    if (!Number.isFinite(target) || target <= 0) return;
    mode = 'custom';
    scale = Math.max(0.25, Math.min(4, target));
  }
  void render(anchor);
}
function turn(delta) {
  if (!pdf) return;
  const target = Math.max(0, Math.min(pdf.numPages - 1, page + delta));
  if (target === page) return;
  page = target;
  void render();
}
addEventListener('message', event => {
  if (event.source !== parent || typeof event.data !== 'string') return;
  if (event.data === 'yomu-pdf-turn:1') turn(1);
  if (event.data === 'yomu-pdf-turn:-1') turn(-1);
  if (event.data.startsWith('yomu-pdf-zoom:')) setZoom(event.data.slice('yomu-pdf-zoom:'.length));
});
addEventListener('keydown', event => {
  if (event.altKey || event.metaKey || event.ctrlKey) return;
  if (event.key === 'ArrowRight' || event.key === 'PageDown') { event.preventDefault(); turn(1); }
  if (event.key === 'ArrowLeft' || event.key === 'PageUp') { event.preventDefault(); turn(-1); }
});
addEventListener('wheel', event => {
  if (!event.ctrlKey) return;
  event.preventDefault();
  setZoom(scale * Math.exp(-event.deltaY * 0.002), captureAnchor(event.clientX, event.clientY));
}, { passive: false });

// Native vertical scrolling (and horizontal panning while enlarged) stays
// available. Only fitted pages accept horizontal turn gestures. Mouse input
// remains ordinary text selection; we never replace it with drag-to-pan.
let swipe, pinch, blocked = false;
function distance(touches) { return Math.hypot(touches[0].clientX - touches[1].clientX, touches[0].clientY - touches[1].clientY); }
addEventListener('touchstart', event => {
  if (!pdf) return;
  if (event.touches.length === 2) {
    swipe = null; blocked = true;
    const x = (event.touches[0].clientX + event.touches[1].clientX) / 2;
    const y = (event.touches[0].clientY + event.touches[1].clientY) / 2;
    pinch = { distance: distance(event.touches), scale, target: scale, anchor: captureAnchor(x, y) };
    event.preventDefault();
  } else if (event.touches.length === 1 && !blocked && !document.documentElement.classList.contains('zoomed')) {
    const selection = getSelection();
    if (selection && !selection.isCollapsed) return;
    const touch = event.touches[0];
    swipe = { x: touch.clientX, y: touch.clientY, at: performance.now(), dx: 0, dy: 0 };
  } else { swipe = null; }
}, { passive: false });
addEventListener('touchmove', event => {
  if (pinch && event.touches.length === 2) {
    event.preventDefault();
    if (pinch.distance <= 0) return;
    pinch.target = Math.max(0.25, Math.min(4, pinch.scale * distance(event.touches) / pinch.distance));
    // Preview the pinch immediately, then rerasterize once on release. Do not
    // leave the text/canvas permanently CSS-scaled (which would blur them).
    container.style.transformOrigin = '0 0';
    container.style.transform = `scale(${pinch.target / pinch.scale})`;
  } else if (swipe && event.touches.length === 1) {
    swipe.dx = event.touches[0].clientX - swipe.x;
    swipe.dy = event.touches[0].clientY - swipe.y;
    if (Math.abs(swipe.dx) > 16 && Math.abs(swipe.dx) > Math.abs(swipe.dy) * 2) event.preventDefault();
  }
}, { passive: false });
addEventListener('touchend', event => {
  if (pinch) {
    container.style.transform = '';
    setZoom(pinch.target, pinch.anchor);
    pinch = null;
  } else if (!blocked && swipe && event.touches.length === 0 && performance.now() - swipe.at < 700
      && Math.abs(swipe.dx) > 64 && Math.abs(swipe.dx) > Math.abs(swipe.dy) * 2) {
    turn(swipe.dx < 0 ? 1 : -1);
  }
  swipe = null;
  if (event.touches.length === 0) blocked = false;
});
addEventListener('touchcancel', () => {
  container.style.transform = '';
  swipe = null; pinch = null; blocked = false;
});
let resize;
addEventListener('resize', () => {
  clearTimeout(resize);
  const anchor = captureAnchor();
  resize = setTimeout(() => void render(anchor), 100);
});
try {
  const url = new URL(params.get('file'));
  if (!['http:', 'https:'].includes(url.protocol) || !url.pathname.includes('/api/v1/publications/')) throw new Error('Invalid publication resource URL');
  const root = new URL('./reader-assets/pdfjs/', import.meta.url);
  pdf = await getDocument({
    url: url.href,
    cMapUrl: new URL('cmaps/', root).href, cMapPacked: true,
    standardFontDataUrl: new URL('standard_fonts/', root).href,
    wasmUrl: new URL('wasm/', root).href,
    iccUrl: new URL('iccs/', root).href,
    isEvalSupported: false,
    enableXfa: false,
  }).promise;
  page = Math.min(pdf.numPages - 1, page);
  await render();
} catch (error) {
  status.textContent = `Could not open PDF: ${error.message}`;
  notify(`yomu-reader-error:${status.textContent}`);
}
