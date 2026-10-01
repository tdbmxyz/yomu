// PDF.js is lazy-loaded only when a PDF is opened. Both local builds and Nix
// use the exact same pinned distribution; never fetch reader code from a CDN.
import { cpSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const source = process.env.YOMU_PDFJS_DIST || resolve(root, 'node_modules/pdfjs-dist');
const version = JSON.parse(readFileSync(resolve(source, 'package.json'), 'utf8')).version;
const expected = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8')).dependencies['pdfjs-dist'];
if (version !== expected) throw new Error(`PDF.js version mismatch: ${version}, expected ${expected}; run npm ci`);
const assets = resolve(root, 'crates/yomu-web/reader-assets');
const target = resolve(assets, 'pdfjs');
// Never let files removed by a PDF.js upgrade linger in a local build and get
// copied into dist. Nix starts clean already; local Trunk builds must match it.
rmSync(assets, { recursive: true, force: true });
mkdirSync(target, { recursive: true });
cpSync(resolve(root, 'crates/yomu-server/src/api/reader-interaction.js'),
  resolve(assets, 'reader-interaction.js'));
for (const file of ['pdf.mjs', 'pdf.worker.mjs']) {
  cpSync(resolve(source, 'legacy/build', file), resolve(target, file));
}
for (const dir of ['cmaps', 'standard_fonts', 'wasm', 'iccs']) {
  cpSync(resolve(source, dir), resolve(target, dir), { recursive: true });
}
cpSync(resolve(source, 'LICENSE'), resolve(target, 'LICENSE'));
