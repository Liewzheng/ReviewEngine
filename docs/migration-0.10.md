# Migrating to review-engine 0.10

0.10.0 adds a persistent storage layer: review history, git platform configs, and LLM provider configs now live in a database (PostgreSQL when `DATABASE_URL` is set, otherwise an embedded SQLite file) instead of only in memory and `ui-state.toml`.

**Read this first:**

- The upgrade itself is **fully automatic** — the first 0.10.x boot migrates everything for you. No manual data migration is needed.
- The upgrade is **one-way**. Downgrading back to 0.9.x is **not supported** (see [Downgrading is not supported](#downgrading-is-not-supported)).
- **Back up your config directory before upgrading.** It is your only way back if anything goes wrong.

---

## Before you upgrade

### 1. Back up the config directory

Back up the **entire config directory**, not just one file:

- Plain binary / Homebrew install: `~/.config/review-engine/` (or `$REVIEW_ENGINE_CONFIG_DIR` if you override it).
- Docker (standalone compose): the `./config` and `./auth` bind-mount directories next to your `docker-compose.yml` (`deploy/standalone-compose.yml` mounts them at `/app/config` and `/app/auth`).

```bash
cp -a ~/.config/review-engine ~/.config/review-engine.backup-0.9
```

The directory holds everything 0.9.x needs to reconstruct your setup:

| File | What it is |
|---|---|
| `ui-state.toml` | Web-UI managed config: git platforms, LLM providers, rules, UI projection. |
| `secrets.key` | The ChaCha20-Poly1305 key that encrypts `enc:` credentials. **Without it, encrypted credentials are unrecoverable** — never exclude it from the backup. |
| `auth.toml` | SHA-256 digest of your API token (in Docker deployments this lives in the `./auth` volume). |
| `.code-audit-config.toml` | The static CLI/server config file. |
| `reports/` | Saved review reports (untouched by the migration, but back them up anyway). |

> 0.9.x never created a database, so there is no old database to preserve and no schema conflict — the backup above is all you need.

### 2. If you set `DATABASE_URL`, check it now

With 0.10.0, a `DATABASE_URL` that points at an unreachable PostgreSQL is a **hard startup error** — the server refuses to boot rather than silently fall back to SQLite (your data must never land in an unexpected place). Before restarting onto 0.10.x, confirm the database is reachable from the server host. If you previously had a stray `DATABASE_URL` in the environment that you never used, unset it or expect startup to fail until you do (see [Troubleshooting](#troubleshooting)).

---

## Upgrade

Use whichever path matches your install (all of them are the same binary; the migration runs on the first 0.10.x boot, not during install):

- **Homebrew**: `brew upgrade review-engine`, then restart the server.
- **Docker**: `docker pull ghcr.io/liewzheng/review-engine:latest` (mainland China: pull from the `ghcr.nju.edu.cn` mirror and re-tag, see [`getting-started.md`](getting-started.md#docker含国内加速)) and recreate the container (`docker compose up -d`). The in-container self-upgrade (web UI **Upgrade** button, or `POST /api/v1/system/upgrade`) works too — the container restarts itself with the new binary.
- **Plain binary**: `reng upgrade` (or re-run `install.sh`).

Nothing else changes: ports, auth, webhooks, and your `.code-audit-config.toml` all carry over.

---

## What happens on the first 0.10.x boot

All four steps run automatically at startup, in this order. There is nothing for you to trigger.

1. **Connect + create the database.** `DATABASE_URL` set (and starting with `postgres://`/`postgresql://`) → PostgreSQL; unset → an embedded SQLite database created at `<config-dir>/review.db` (e.g. `~/.config/review-engine/review.db`). A set-but-unreachable `DATABASE_URL` aborts startup with an explicit error — this step never silently falls back.
2. **Apply schema migrations.** The migration that creates the 7 tables (`reviews`, `expert_reports`, `mr_discussions`, `review_contexts`, `git_platforms`, `llm_providers`, `app_settings`) is compiled into the binary and applied here. Migrations are idempotent: first boot creates the tables, every later boot skips them. A migration failure aborts startup before HTTP comes up.
3. **One-shot import of `ui-state.toml`.** If — and only if — the three config tables are completely empty and `ui-state.toml` exists, its contents are imported into the database in a single transaction. On success the file is renamed to `ui-state.toml.migrated` (kept as a backup, never deleted) and you will see:

   ```text
   INFO imported ui-state.toml into the database; backup at <config-dir>/ui-state.toml.migrated
   ```

   All credentials — including LLM API keys, which 0.9.x stored in plaintext — are written to the database `enc:`-encrypted with the same `secrets.key` as before.
4. **Replay config from the database.** Configuration is applied from the database through the same code path the web UI has always used, and you will see `INFO applied UI state from the database`. From now on the database is the authoritative source and `PUT /api/v1/config` persists to it.

Two other log lines you may see on that first boot:

- `WARN marked N interrupted review task(s) as failed (server restarted); they can be retried manually from the history page` — reviews that were pending/running when the old process stopped are closed as `failed` with `error='interrupted: server restarted'`. They are **not** re-run automatically (that would burn LLM quota and could double-post MR comments); retry them manually from the History page.
- `WARN persistence disabled via REVIEW_DISABLE_DB — running with 0.9 in-memory + file behaviour` — only if you set the escape hatch (see [Troubleshooting](#troubleshooting)).

---

## Verify the upgrade

1. **Storage backend is active.** The health endpoint now reports which backend is in use:

   ```bash
   curl -s http://<host>:<port>/api/v1/system/health | jq .storage_backend
   ```

   Expect `"postgresql"` (with `DATABASE_URL`) or `"sqlite"` (embedded). `"disabled"` means the server is running in 0.9 mode — check whether `REVIEW_DISABLE_DB` is set or the config directory could not be resolved. The Configuration page's Advanced card shows the same value as a read-only row.

2. **Configuration survived.** Open the web UI Configuration page: your git platforms and LLM providers should be exactly as before, and webhooks/reviews should work without re-entering anything.

3. **History persists.** Run any review, restart the server, and confirm the entry is still on the History page. (Under 0.9.x, history vanished on restart and was bounded by a 30-minute in-memory window; it is now served from the database.)

4. **The file was archived.** `ui-state.toml` should now be `ui-state.toml.migrated` in the config directory, alongside the new `review.db` (SQLite installs).

---

## Troubleshooting

**Startup fails with "DATABASE_URL is set but the database is unreachable"**
This is deliberate fail-fast behaviour — the server refuses to boot rather than write your data into an unexpected embedded SQLite file. Three ways out:

1. Fix the PostgreSQL connection (host, credentials, network) and start again — preferred.
2. Unset `DATABASE_URL` to use the embedded SQLite database instead.
3. Set `REVIEW_DISABLE_DB=1` to bypass persistence entirely and run with exact 0.9 behaviour (in-memory history, config in `ui-state.toml`). Accepted values are `1`, `true`, or `yes` (case-insensitive). Use this only as a temporary escape hatch — review history will not survive restarts while it is set.

**The log shows "ui-state.toml import failed … the file is untouched"**
The import is a single transaction: a mid-import failure rolls everything back, leaves `ui-state.toml` exactly where it was, and the server keeps starting by replaying the file (0.9 behaviour) so you never lose your configuration. Fix the cause shown in the error and restart — the import retries automatically, because it only runs while the config tables are still empty.

**`secrets.key` was lost**
Credentials stored `enc:`-encrypted in the database (git tokens, webhook secrets, LLM API keys) cannot be decrypted without it. Re-enter the credentials in the web UI Configuration page and save — new values are encrypted under a fresh key. (This is the same threat model as 0.9.x, which is why the backup above must include `secrets.key`.)

**A review was interrupted by the upgrade restart**
It appears on the History page as `failed` with `error='interrupted: server restarted'`. Use retry from the History page to re-run it.

---

## Downgrading is not supported

0.10.x is a one-way move: after the first boot, your configuration's authoritative source is the database and `ui-state.toml` has been renamed to `ui-state.toml.migrated`. A 0.9.x binary does not read the database, so rolling the binary back would start 0.9.x with **no configuration** — do not treat a version rollback as a supported operation.

If a genuine disaster forces you back to 0.9.x, the pieces for a *manual* recovery are the config-directory backup you took before upgrading and the untouched `ui-state.toml.migrated` (renaming it back to `ui-state.toml` restores the old file-based config source). The database itself is inert for 0.9.x and can be left in place or deleted. This is a last-resort recovery procedure, not a supported downgrade path — and it only works if you made the backup.

---

## Related reading

- [`CHANGELOG.md`](../CHANGELOG.md) — full 0.10.0 release notes.
- [`design/persistence.md`](../design/persistence.md) — the persistence design (schema, startup sequence, risk table) this guide is based on.
- [`configuration.md`](configuration.md) — full configuration reference.
- [`faq.md`](faq.md) — API token and deployment troubleshooting.
