//! Consistent SQLite snapshots and recovery into a fresh, private directory.
//! Maintenance never loads keys, replaces an existing database, or sends notifications.
use crate::MIGRATIONS;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("maintenance filesystem operation failed; inspect isolated partial output")]
    Io,
    #[error("maintenance database operation failed; original database was not replaced")]
    Database,
    #[error("backup integrity or manifest verification failed")]
    Invalid,
    #[error("database migration history is incompatible with this binary")]
    Version,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}

async fn connect(path: &Path, read_only: bool) -> Result<SqliteConnection, Error> {
    Ok(SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .read_only(read_only)
            .foreign_keys(true)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5)),
    )
    .await?)
}

fn private_dir(path: &Path) -> Result<(), Error> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
fn new_file(path: &Path) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn digest(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
async fn validate(conn: &mut SqliteConnection) -> Result<Vec<i64>, Error> {
    let checks: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut *conn)
        .await?;
    if checks != ["ok"]
        || !sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await?
            .is_empty()
    {
        return Err(Error::Invalid);
    }
    let history: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await?;
    let embedded: Vec<_> = MIGRATIONS
        .iter()
        .map(|m| (m.version, m.checksum.as_ref()))
        .collect();
    validate_history(&history, &embedded)?;
    Ok(history.into_iter().map(|(v, _, _)| v).collect())
}
fn validate_history(
    history: &[(i64, bool, Vec<u8>)],
    embedded: &[(i64, &[u8])],
) -> Result<(), Error> {
    if history.is_empty() {
        return Err(Error::Version);
    }
    for (version, success, checksum) in history {
        if !success
            || !embedded
                .iter()
                .any(|(v, c)| v == version && *c == checksum.as_slice())
        {
            return Err(Error::Version);
        }
    }
    Ok(())
}

async fn snapshot(source: &Path, target: &Path) -> Result<Vec<i64>, Error> {
    new_file(target)?;
    let mut conn = connect(source, true).await?;
    sqlx::query("VACUUM INTO ?")
        .bind(target.to_str().ok_or(Error::Io)?)
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    let mut copy = connect(target, true).await?;
    let versions = validate(&mut copy).await?;
    copy.close().await?;
    File::open(target)?.sync_all()?;
    Ok(versions)
}

/// Publish a verified WAL-aware snapshot and manifest in a new private directory.
/// Existing destinations are always rejected. A failure may leave quarantined partial output.
pub async fn backup(source: &Path, destination: &Path) -> Result<(), Error> {
    // Opening first prevents a typo from creating a source database or output directory.
    connect(source, true).await?.close().await?;
    private_dir(destination)?;
    let partial = destination.join("snapshot.partial");
    let versions = snapshot(source, &partial).await?;
    let manifest = json!({"format":1,"migrations":versions,"sha256":digest(&partial)?});
    fs::rename(&partial, destination.join("snapshot.sqlite3"))?;
    let mut file = new_file(&destination.join("manifest.partial"))?;
    file.write_all(
        serde_json::to_string_pretty(&manifest)
            .map_err(|_| Error::Invalid)?
            .as_bytes(),
    )?;
    file.sync_all()?;
    fs::rename(
        destination.join("manifest.partial"),
        destination.join("manifest.json"),
    )?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

/// Verify the manifest, take another consistent copy, upgrade it, and publish a paused database.
/// `cutoff` is UTC Unix milliseconds: reminders strictly before it remain suppressed after resume.
/// The operator must keep this directory isolated from serving processes until inspection finishes.
pub async fn restore(backup: &Path, destination: &Path, cutoff: i64) -> Result<(), Error> {
    let mut bytes = Vec::new();
    File::open(backup.join("manifest.json"))?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::Invalid);
    }
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    if manifest["format"] != 1 {
        return Err(Error::Invalid);
    }
    let source = backup.join("snapshot.sqlite3");
    private_dir(destination)?;
    let partial = destination.join("database.partial");
    // Copy the exact standalone artifact before hashing it, avoiding a source hash/copy race.
    let mut output = new_file(&partial)?;
    std::io::copy(&mut File::open(&source)?, &mut output)?;
    output.sync_all()?;
    if manifest["sha256"].as_str() != Some(digest(&partial)?.as_str()) {
        return Err(Error::Invalid);
    }
    let mut check = connect(&partial, true).await?;
    let versions = validate(&mut check).await?;
    check.close().await?;
    if manifest["migrations"] != json!(versions) {
        return Err(Error::Invalid);
    }
    let mut conn = connect(&partial, false).await?;
    MIGRATIONS
        .run(&mut conn)
        .await
        .map_err(|_| Error::Version)?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE recovery_control SET paused=1,cutoff=MAX(COALESCE(cutoff,?),?) WHERE id=1")
        .bind(cutoff)
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
    let cutoff: i64 = sqlx::query_scalar("SELECT cutoff FROM recovery_control WHERE id=1")
        .fetch_one(&mut *tx)
        .await?;
    // Preserve sent/accepted audit. Old in-flight attempts cannot be trusted after a restore.
    sqlx::query("UPDATE delivery_attempts SET finished_at=?,outcome=CASE WHEN sent_at IS NULL THEN 'not_sent' ELSE 'unknown' END,disposition=CASE WHEN (SELECT state FROM delivery_jobs WHERE id=job_id) NOT IN ('pending','retry_wait','leased') THEN (SELECT state FROM delivery_jobs WHERE id=job_id) WHEN (SELECT due_at FROM delivery_jobs WHERE id=job_id)<? THEN 'expired' ELSE 'retry_wait' END WHERE finished_at IS NULL")
        .bind(cutoff).bind(cutoff).execute(&mut *tx).await?;
    sqlx::query("UPDATE delivery_jobs SET state=CASE WHEN due_at<? THEN 'expired' ELSE 'retry_wait' END,lease_owner=NULL,lease_token=NULL,lease_until=NULL WHERE state IN ('pending','retry_wait','leased')")
        .bind(cutoff).execute(&mut *tx).await?;
    sqlx::query("UPDATE delivery_jobs SET lease_owner=NULL,lease_token=NULL,lease_until=NULL WHERE lease_token IS NOT NULL OR lease_owner IS NOT NULL OR lease_until IS NOT NULL")
        .execute(&mut *tx).await?;
    tx.commit().await?;
    validate(&mut conn).await?;
    conn.close().await?;
    File::open(&partial)?.sync_all()?;
    fs::rename(&partial, destination.join("database.sqlite3"))?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

/// Explicitly resume an inspected recovery. The historical cutoff cannot be removed here.
/// Run while isolated/stopped; this is not a server lifecycle or online management API.
pub async fn resume(database: &Path) -> Result<(), Error> {
    let mut conn = connect(database, false).await?;
    validate(&mut conn).await?;
    let changed =
        sqlx::query("UPDATE recovery_control SET paused=0 WHERE id=1 AND cutoff IS NOT NULL")
            .execute(&mut conn)
            .await?
            .rows_affected();
    if changed != 1 {
        return Err(Error::Invalid);
    }
    conn.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_migrations_can_arrive_out_of_order_without_invalidating_known_backups() {
        // Reserved numbers may merge after 0009: match known versions, not a positional prefix.
        let applied = vec![(1, true, vec![1]), (9, true, vec![9])];
        let embedded: Vec<(i64, &[u8])> = vec![(1, &[1]), (7, &[7]), (8, &[8]), (9, &[9])];
        assert!(validate_history(&applied, &embedded).is_ok());
        for bad in [
            vec![(99, true, vec![99])],
            vec![(9, false, vec![9])],
            vec![(9, true, vec![0])],
        ] {
            assert!(matches!(
                validate_history(&bad, &embedded),
                Err(Error::Version)
            ));
        }
    }
}
