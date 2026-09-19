import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { brotliCompressSync, constants } from 'node:zlib';

const dir = resolve(process.argv[2] || 'crates/yomu-web/dist');
const html = readFileSync(resolve(dir, 'index.html'), 'utf8');
const files = new Set(['index.html', 'sw.js']);
for (const match of html.matchAll(/(?:href|src|from)\s*=?\s*["']([^"']+\.(?:js|css|wasm))["']/g)) {
  const url = new URL(match[1], 'https://build.invalid/');
  if (url.origin !== 'https://build.invalid') throw new Error('External boot asset needs a size/offline policy');
  files.add(decodeURIComponent(url.pathname.slice(1)));
}
if (![...files].some(f => f.endsWith('.wasm'))) throw new Error('No WASM in shell; refusing a vacuous budget check');
const assets = [...files].sort().map(file => {
  const path = resolve(dir, file);
  if (!path.startsWith(`${dir}/`)) throw new Error('Asset outside dist');
  const bytes = readFileSync(path);
  return { file, raw: bytes.length, brotli: brotliCompressSync(bytes, {
    params: { [constants.BROTLI_PARAM_QUALITY]: 11 },
  }).length };
});
// Baseline before offline hardening: ~1.67 MB raw / 505 KB Brotli, including SW.
// Leave deliberate headroom for reliability fixes, not a multi-megabyte regression.
const budget = { raw: 2_000_000, brotli: 600_000 };
const total = assets.reduce((sum, a) => ({ raw: sum.raw + a.raw, brotli: sum.brotli + a.brotli }), { raw: 0, brotli: 0 });
console.log(JSON.stringify({ assets, total, budget }, null, 2));
if (total.raw > budget.raw || total.brotli > budget.brotli) {
  console.error('Frontend size budget exceeded; investigate before adjusting reviewed ceilings.');
  process.exitCode = 1;
}
