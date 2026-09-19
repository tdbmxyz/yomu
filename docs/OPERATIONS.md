# Operations and database maintenance

## Upgrade notes: offline ownership and source networking

- No released server migration is rewritten by this hardening change. Historical
  migrations/restarts are tested; still take a consistent backup before deploying.
- Reload browser tabs and update native apps together with the web bundle. Do the
  first client launch online so it can establish the server/account boundary.
  Close old reader tabs before upgrading the Service Worker; do not clear site data.
- Existing unscoped client state and legacy browser caches are retained. In OIDC
  mode they are **not** assigned to whichever account happens to sign in first.
  Settings → Offline storage can export retained state and, with explicit owner
  confirmation, import legacy data. Check the account and server before importing.
  This confirmation is required in shared-account mode too: the current auth mode
  cannot prove the owner of older unscoped work. A banner flags retained pending
  history so it does not silently disappear from the user's workflow.
  The offline-storage export is a technical recovery file, not the library/progress
  Backup import format. Keep it private: it contains retained reading state.
- Do not blindly downgrade clients: older clients can replay retained unscoped
  work. Export retained state first and validate any rollback in isolation; do not
  clear browser storage or restore a server backup over newer production history.
- Logout purges that account's browser response/page caches; pending history stays
  scoped to its owner and resumes on sign-in to the same account. Cache eviction
  reconciles saved badges rather than claiming missing pages remain available.
- Selector fetches validate every resolved address, pin the vetted addresses for
  the connection, check redirects and bound HTML (8 MiB) and images (32 MiB).
  Ambient HTTP(S)_PROXY settings are no longer used, since they can bypass the
  resolver. Validate any proxy-dependent deployment in staging before rollout.
- Private-network sources must explicitly list exact `allowed_private_hosts` in
  their TOML. Leave this empty for public sites. It permits **only those hosts**
  (including on redirects), not arbitrary LAN targets. Do not whitelist untrusted
  names to work around failures. No production definitions are edited automatically.

## Health endpoints

- `GET /api/v1/health` is liveness and app sign-in discovery. It intentionally
  stays cheap and answers when dependencies are degraded.
- `GET /api/v1/health/readiness` acquires a SQLite writer, probes `data_dir`,
  verifies the configured books directory is readable, reports free bytes, and
  enforces `operations.minimum_free_bytes`. Use it as the readiness check.
- `GET /api/v1/metrics` emits Prometheus text for uptime, request count, SQLite
  pool use, free data bytes, and expired-session cleanup.

Readiness and metrics contain no library/user data and are unauthenticated so a
local supervisor can use them even when OIDC is unavailable. Restrict them at
the reverse proxy if the server is internet-facing.

```toml
[operations]
minimum_free_bytes = 536870912       # 512 MiB; zero disables the floor
maintenance_interval_secs = 21600   # zero disables periodic maintenance
```

The maintenance task deletes expired sessions and requests a non-blocking
`PRAGMA wal_checkpoint(PASSIVE)`. Expired sessions are also cleaned on login.

## Backups

Back up both **SQLite** and downloaded content. The JSON export in the UI is a
portable library/progress backup but intentionally does not contain page files.
For a filesystem-level backup:

1. Create a consistent SQLite snapshot while yomu is running:

   ```bash
   sqlite3 /var/lib/yomu/yomu.db \
     ".timeout 10000" \
     ".backup '/var/backups/yomu/yomu-$(date -u +%Y%m%dT%H%M%SZ).db'"
   ```

   Do not copy only `yomu.db` while WAL mode is active; committed data may still
   be in `yomu.db-wal`. SQLite's backup command handles this correctly.
2. Back up `data_dir` (downloaded pages and covers), the books directory if yomu
   is its owner, configuration, and source definitions with the snapshot.
3. Encrypt off-host copies and test restoring into a scratch instance. A backup
   that has never been restored is unverified.

A systemd timer, restic/borg job, or ZFS/Btrfs snapshot can automate this. Run it
as a principal that can read yomu's state; the hardened DynamicUser service
itself should not receive broad backup-directory access.

## Automated restore drills

`just test-recovery` creates synthetic file-backed historical databases (before
publication conversion, shared-history transfer, and identity aliases), migrates
with the real SQLx migrator, takes a live SQLite snapshot, changes the original,
and reopens the restored copy twice. It verifies history, read marks, downloads,
sessions, aliases, integrity and foreign keys. It runs in CI and never reads
an installation's data. This complements—not replaces—periodic operator restore
drills of encrypted backups, page files, books, config and source definitions.

Before upgrading a daily-use installation, restore a backup generation into an
isolated scratch instance with updater/notifications disabled and no real source
traffic. Check representative saved chapters and reading positions. Keep the
previous binary and untouched backup; never test a downgrade on the live DB.

## Checks and recovery

The binary provides bounded maintenance commands using the configured DB:

```bash
YOMU_CONFIG=/etc/yomu.toml yomu-server integrity-check
YOMU_CONFIG=/etc/yomu.toml yomu-server checkpoint
YOMU_CONFIG=/etc/yomu.toml yomu-server cleanup-sessions
```

Run `integrity-check` after an unclean storage failure and periodically during a
backup verification job. It exits non-zero unless SQLite returns `ok`. Stop yomu
before replacing/restoring its database and restore the DB and data directory
from the same backup generation. Keep the original files until the restored
instance passes integrity and readiness checks.
