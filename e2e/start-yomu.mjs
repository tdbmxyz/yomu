import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const state = resolve(import.meta.dirname, '.state');
rmSync(state, { recursive: true, force: true });
mkdirSync(resolve(state, 'data'), { recursive: true });
mkdirSync(resolve(state, 'books'), { recursive: true });
const config = resolve(state, 'yomu.toml');
writeFileSync(config, `
listen = "127.0.0.1:4711"
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

const binary = process.env.YOMU_E2E_SERVER;
const child = spawn(binary || 'cargo', binary ? [] : ['run', '--locked', '-p', 'yomu-server'], {
  cwd: root,
  env: { ...process.env, YOMU_CONFIG: config, RUST_LOG: 'warn' },
  stdio: 'inherit',
});
child.on('exit', code => process.exit(code ?? 1));
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => child.kill(signal));
}
