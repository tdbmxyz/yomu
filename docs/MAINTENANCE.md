# Maintenance and release checks

## Safety rules

- Development tests use synthetic data, temporary databases, and the E2E source/IdP.
  Never point them at a running installation or copy its credentials into fixtures.
- Do not rewrite released SQL migrations. Add migrations when schema changes are
  necessary; keep old wire formats readable and test upgrade + restart.
- Back up SQLite, downloaded pages, books, configuration, and source definitions
  together before deploying. Follow [OPERATIONS.md](OPERATIONS.md); retain the old
  binary and backup generation. Do not downgrade a migrated database in place.
- Browser/native storage upgrades must preserve unsynced reading history. An
  unknown owner is a reason to quarantine data, not to guess an account or delete it.
- Security fixes that reject previously accepted source URLs require a staging
  check of operator-owned definitions. Never automatically enable private-network
  access to make a broken source work.

## Checks

`just check`, `just test`, `just e2e`, and `just deny` cover Rust, browser journeys,
fixtures, and dependency policy. Desktop and Android checks run separately in CI.
The database recovery tests exercise historical migrations, file-backed reopen,
SQLite snapshots, identity reconciliation, and idempotent startup without any
production data. `just test-recovery` runs them alone.

`node scripts/check-web-size.mjs crates/yomu-web/dist` reports raw and Brotli
bytes for the shell and its referenced assets and enforces reviewed size budgets.
CI retains the report. Budgets are ceilings, not optimization targets: investigate
unexpected growth before updating them, and explain intentional increases in the PR.

## Documentation policy

Keep README for onboarding, ARCHITECTURE for current invariants, OPERATIONS for
recovery, ROADMAP for unshipped work, ADRs for decisions, and the short auth/delivery
checklists for known pitfalls. Update the relevant document with behavior changes.
Do not retain completed task plans, copied code walkthroughs, stale handoffs, or
personal deployment notes. Their history remains available in Git (the old
`docs/superpowers/`, `HANDOFF.md`, and keep-alive porting notes were retired).
Avoid duplicating API/config details already maintained in code and examples.
