# SQLx 0.9 compatibility (#62)

The workspace uses SQLite with Tokio, embedded migrations and derives. SQLx 0.9 requires Rust 1.94 or newer (upstream supported MSRV); verification uses Rust 1.99.1. The project does not pin an older MSRV. Keep CalRelay's Cargo.lock and all `--locked` gates even though upstream no longer tracks its own lockfile.

SQLx now accepts static SQL or an explicit SqlSafeStr assertion. The only formatted query consisted entirely of a fixed SELECT and a fixed WHERE clause. It is now one static JOB_BY_ID string; the job ID is still bound with `?`. No production dynamic SQL assertion or string interpolation of input is introduced. Test-only count, PRAGMA and generation queries likewise select static SQL from explicit matches; two test-only trigger builders explicitly use AssertSqlSafe after auditing their fixed table-name lists.

Upstream also changes Migrate, migrator setter return types, SQLite value Send/Sync and mixed borrowed decoding rules, and optional extension/serialize/unlock-notify features. CalRelay uses the embedded migrator and owned row fields, implements no custom Migrate trait, and neither loads SQLite extensions nor serializes raw connections. New sqlx.toml and no_tx migrations are not enabled. Migration SQL, checksums, WAL, FULL synchronous mode, foreign keys and five-second busy timeout remain unchanged. PostgreSQL/MySQL/Any-specific changes do not apply to the enabled driver. The lockfile retains libsqlite3-sys 0.30.1; the SQLite range change does not silently upgrade the engine here.

Verification maps the dependency boundary to existing real SQLite integration tests: historical schemas to latest, rejected corrupt/changed histories, reopen/reentry, foreign keys, source CAS, queue leases, transaction rollback, contention and process-crash recovery. Existing tests also exercise parameter binding in the delivery path. The initial 0.9 build failed on SqlSafeStr before the static-query fix; that compiler failure is compatibility evidence, not a behavior TDD Red. No new schema or product behavior is required.

Official sources checked:

- [SQLx 0.9.0 changelog](https://github.com/transact-rs/sqlx/blob/v0.9.0/CHANGELOG.md), including SqlStr, SQLite safety changes, migration configuration and MSRV.
- [SQL string API source](https://github.com/transact-rs/sqlx/blob/v0.9.0/sqlx-core/src/sql_str.rs).
- [MSRV policy](https://github.com/transact-rs/sqlx/blob/v0.9.0/FAQ.md#msrv).

PR #65 is still separate and unmerged. Its calendar-server workspace member and additional lockfile dependencies must be preserved when the maintainer later integrates branches; this PR does not include that feature code.
