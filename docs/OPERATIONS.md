# Operations and database maintenance

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
