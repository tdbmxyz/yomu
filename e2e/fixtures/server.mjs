import http from 'node:http';
import { URL } from 'node:url';

const host = '127.0.0.1';
const port = 4811;
const codes = new Map();
let orchardBlocked = false;
let solverCalls = 0;
let solverImageCalls = 0;
const solverCatalog = '<div class="manga-item"><a class="manga-link" href="/solver/title"><img src="/covers/fixture.png">Solver Comic</a></div>';
const solverTitle = () => manga().replaceAll('Fixture Farming', 'Solver Comic').replaceAll('/manga/fixture/chapter-', '/solver/chapter/');
const solverChapter = () => chapter().replaceAll('/pages/', '/solver/page/');
const png = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
  'base64',
);

const escape = value => String(value).replace(/[&<>"']/g, c => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
}[c]));

function html(res, body, status = 200) {
  res.writeHead(status, { 'content-type': 'text/html; charset=utf-8' });
  res.end(`<!doctype html><html><body>${body}</body></html>`);
}

function redirect(res, location) {
  res.writeHead(302, { location });
  res.end();
}

function manga(marathon = false) {
  if (marathon) return `<h1 class="entry-title">Fixture Marathon</h1>
    <div class="summary">Twelve chapters for continuous reading.</div>
    <ul class="chapters">${Array.from({ length: 12 }, (_, i) =>
      `<li class="chapter"><a href="/manga/marathon/chapter-${12 - i}">Chapter ${12 - i}</a></li>`).join('')}</ul>`;
  return `<h1 class="entry-title">Fixture Farming</h1>
    <div class="summary">A deterministic comic for browser tests.</div>
    <div class="cover"><img src="/covers/fixture.png"></div>
    <ul class="chapters">
      <li class="chapter"><a href="/manga/fixture/chapter-3">Chapter 3</a><time datetime="2026-01-03T12:00:00Z">Jan 3</time></li>
      <li class="chapter"><a href="/manga/fixture/chapter-2">Chapter 2</a></li>
      <li class="chapter"><a href="/manga/fixture/chapter-1">Chapter 1</a></li>
    </ul>`;
}

function chapter() {
  return `<div class="reading-content">
    <img class="page" data-src="http://localhost:4811/pages/001.png">
    <img class="page" data-src="http://localhost:4811/pages/002.png">
    <img class="page" data-src="http://localhost:4811/pages/003.png">
  </div>`;
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${host}:${port}`);
  if (url.pathname === '/__fixture/solver-stats') {
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({ solves: solverCalls, images: solverImageCalls }));
  }
  if (url.pathname === '/v1' && req.method === 'POST') {
    let body = '';
    for await (const chunk of req) body += chunk;
    const command = JSON.parse(body);
    const target = new URL(command.url);
    if (command.cmd !== 'request.get' || target.hostname !== 'localhost' || target.port !== '4811'
        || !['/solver/catalog', '/solver/title'].includes(target.pathname)) {
      res.writeHead(400);
      return res.end();
    }
    solverCalls++;
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({ status: 'ok', solution: {
      url: target.href, status: 200, response: target.pathname === '/solver/catalog' ? solverCatalog : solverTitle(),
      userAgent: 'FixtureSolverBrowser',
      cookies: [{ name: 'solver_clearance', value: 'fixture', domain: 'localhost', path: '/', secure: false, expires: -1 }],
    } }));
  }
  if (url.pathname === '/solver/search' && url.searchParams.get('q') !== 'Solver') return html(res, '');
  if (url.pathname.startsWith('/solver/')) {
    if (req.headers['user-agent'] !== 'FixtureSolverBrowser' || !req.headers.cookie?.includes('solver_clearance=fixture')) {
      res.writeHead(403, { 'cf-mitigated': 'challenge', 'content-type': 'text/html' });
      return res.end('<title>Browser check</title>');
    }
    if (url.pathname.startsWith('/solver/page/')) {
      solverImageCalls++;
      res.writeHead(200, { 'content-type': 'image/png' });
      return res.end(png);
    }
    if (url.pathname.startsWith('/solver/chapter/')) return html(res, solverChapter());
    if (url.pathname === '/solver/title') return html(res, solverTitle());
    return html(res, solverCatalog);
  }
  if (url.pathname === '/.well-known/openid-configuration') {
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({
      authorization_endpoint: `http://${host}:${port}/authorize`,
      token_endpoint: `http://${host}:${port}/token`,
      userinfo_endpoint: `http://${host}:${port}/userinfo`,
    }));
  }
  if (url.pathname === '/authorize') {
    const query = new URLSearchParams(url.searchParams);
    const link = user => {
      query.set('user', user);
      return `/authorize/complete?${query}`;
    };
    return html(res, `<h1>Fixture identity provider</h1>
      <a href="${escape(link('alice'))}">Sign in as Alice</a>
      <a href="${escape(link('bob'))}">Sign in as Bob</a>`);
  }
  if (url.pathname === '/authorize/complete') {
    const user = url.searchParams.get('user') || 'alice';
    const code = `${user}-${Date.now()}-${Math.random()}`;
    codes.set(code, user);
    const callback = new URL(url.searchParams.get('redirect_uri'));
    callback.searchParams.set('code', code);
    callback.searchParams.set('state', url.searchParams.get('state'));
    return redirect(res, callback.toString());
  }
  if (url.pathname === '/token' && req.method === 'POST') {
    let body = '';
    req.on('data', chunk => body += chunk);
    return req.on('end', () => {
      const code = new URLSearchParams(body).get('code');
      if (!code || !codes.has(code)) {
        res.writeHead(400, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: 'invalid_grant' }));
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ access_token: code, token_type: 'Bearer' }));
    });
  }
  if (url.pathname === '/userinfo') {
    const code = (req.headers.authorization || '').replace(/^Bearer /, '');
    const user = codes.get(code);
    if (!user) {
      res.writeHead(401);
      return res.end();
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({
      sub: `fixture-${user}`,
      preferred_username: user,
      name: user === 'alice' ? 'Alice Reader' : 'Bob Reader',
    }));
  }
  if (url.pathname === '/__fixture/orchard-blocked' && req.method === 'POST') {
    orchardBlocked = url.searchParams.get('value') === 'true';
    res.writeHead(204);
    return res.end();
  }
  if (url.pathname === '/covers/blocked.png') return html(res, 'Forbidden', 403);
  if (url.pathname === '/favicon.ico' || url.pathname.startsWith('/covers/') || url.pathname.startsWith('/pages/')) {
    res.writeHead(200, { 'content-type': 'image/png', 'cache-control': 'no-store' });
    return res.end(png);
  }
  if (url.pathname === '/manga/fixture') return html(res, manga());
  if (url.pathname === '/manga/marathon') return html(res, manga(true));
  if (url.pathname === '/manga/orchard') {
    if (orchardBlocked) {
      res.writeHead(403, { 'cf-mitigated': 'challenge', 'content-type': 'text/html' });
      return res.end('<!doctype html><title>Browser check</title>');
    }
    return html(res, manga().replaceAll('Fixture Farming', 'Fixture Orchard')
      .replaceAll('/manga/fixture/', '/manga/orchard/').replace('A deterministic comic for browser tests.', 'A freshly loaded orchard summary.'));
  }
  if (/^\/manga\/(fixture|marathon)\/chapter-\d+$/.test(url.pathname)) return html(res, chapter());
  if (url.pathname === '/' || url.pathname === '/catalog') {
    const page = Number(url.searchParams.get('page') || 1);
    if (page > 2) return html(res, '');
    if (page === 2) return html(res, `<div class="manga-item"><a class="manga-link" href="/manga/fixture">Fixture Farming</a></div>
      <div class="manga-item"><a class="manga-link" href="/manga/orchard"><img src="/covers/blocked.png">Fixture Orchard</a></div>`);
    return html(res, `<div class="manga-item">
      <a class="manga-link" href="/manga/fixture">
        <img src="/covers/fixture.png">Fixture Farming
      </a>
    </div><div class="manga-item"><a class="manga-link" href="/manga/marathon">Fixture Marathon</a></div>`);
  }
  res.writeHead(404);
  res.end('not found');
});

server.listen(port, host, () => console.log(`fixture server on http://${host}:${port}`));
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => server.close(() => process.exit(0)));
}
