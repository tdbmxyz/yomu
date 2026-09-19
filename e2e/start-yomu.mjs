import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';
import { startProxy } from './proxy.mjs';
import { createServer } from 'node:net';

// Refuse an occupied upstream port before opening the proxy. It must never
// forward test mutations to an unrelated (possibly production) listener.
await new Promise((resolve, reject) => {
  const guard = createServer();
  guard.once('error', reject);
  guard.listen(4712, '127.0.0.1', () => guard.close(resolve));
});

const root = resolve(import.meta.dirname, '..');
const state = resolve(import.meta.dirname, '.state');
rmSync(state, { recursive: true, force: true });
mkdirSync(resolve(state, 'data'), { recursive: true });
mkdirSync(resolve(state, 'books'), { recursive: true });
const config = resolve(state, 'yomu.toml');
writeFileSync(config, `
listen = "127.0.0.1:4712"
static_dir = "${resolve(root, 'crates/yomu-web/dist')}"
db_path = "${resolve(state, 'yomu.db')}"
data_dir = "${resolve(state, 'data')}"
sources_dir = "${resolve(root, 'e2e/fixtures')}"

[updater]
enabled = false

[books]
enabled = false
dir = "${resolve(state, 'books')}"

[operations]
minimum_free_bytes = 0
maintenance_interval_secs = 0

[auth]
issuer = "http://127.0.0.1:4811/"
client_id = "yomu-e2e"
public_url = "http://127.0.0.1:4711"
session_days = 1
`);

const proxy = startProxy();
const binary = process.env.YOMU_E2E_SERVER;
// Figment gives YOMU_* environment values precedence over TOML. Never inherit
// an operator's DB, source, notification, auth or listen overrides into tests.
const environment = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
  !key.startsWith('YOMU_') && !/^(https?|all|no)_proxy$/i.test(key)));
const child = spawn(binary || 'cargo', binary ? [] : ['run', '--locked', '-p', 'yomu-server'], {
  cwd: root,
  env: { ...environment, YOMU_CONFIG: config, RUST_LOG: 'warn', NO_PROXY: '*' },
  stdio: 'inherit',
});
child.on('exit', code => process.exit(code ?? 1));
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => { proxy.close(); child.kill(signal); });
}
