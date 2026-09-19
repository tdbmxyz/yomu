// Fixture-only fault proxy. Successful responses always come from real Yomu.
// Never exported by a package or enabled in production.
import http from 'node:http';
import { readFileSync } from 'node:fs';
const faults = [];
let revision = 0;
let legacyWorker = false;
export function startProxy() {
  return http.createServer(async (req, res) => {
    const path = new URL(req.url, 'http://127.0.0.1:4711').pathname;
    if (path === '/__test/control' && req.method === 'POST') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const command = JSON.parse(body);
      if (command.reset) { faults.length = 0; legacyWorker = false; revision = 0; }
      if (command.fault) faults.push(command.fault);
      if (command.workerRevision !== undefined) revision = command.workerRevision;
      if (command.legacyWorker !== undefined) legacyWorker = command.legacyWorker;
      res.writeHead(200, { 'content-type': 'application/json' });
      return res.end('{}');
    }
    if (path === '/sw.js' && legacyWorker) {
      res.writeHead(200, { 'content-type': 'application/javascript', 'cache-control': 'no-store' });
      return res.end(readFileSync(new URL('./fixtures/legacy-worker.js', import.meta.url)));
    }
    const index = faults.findIndex(f => f.path === path && (!f.method || f.method === req.method));
    const fault = index < 0 ? null : faults.splice(index, 1)[0];
    if (fault?.delay) await new Promise(r => setTimeout(r, fault.delay));
    if (fault?.status) {
      res.writeHead(fault.status, { 'content-type': 'application/json', 'retry-after': '1' });
      return res.end(JSON.stringify({ message: 'fixture transient failure' }));
    }
    const upstream = http.request({ hostname: '127.0.0.1', port: 4712, path: req.url, method: req.method, headers: req.headers }, response => {
      if (path === '/sw.js' && revision) {
        const chunks = [];
        response.on('data', chunk => chunks.push(chunk));
        response.on('end', () => {
          const headers = { ...response.headers, 'cache-control': 'no-store' };
          delete headers['content-length']; delete headers.etag; delete headers['last-modified'];
          res.writeHead(response.statusCode, headers);
          res.end(Buffer.concat([...chunks, Buffer.from(`\n// fixture worker revision ${revision}\n`)]));
        });
      } else { res.writeHead(response.statusCode, response.headers); response.pipe(res); }
    });
    upstream.on('error', () => { if (!res.headersSent) res.writeHead(502); res.end(); });
    req.pipe(upstream);
  }).listen(4711, '127.0.0.1');
}
