//! Whole-home backup bundle + restore drill (R8/U4c, F5).
//!
//! A home is a BUNDLE: SQLite metadata index (WAL) + Markdown tree. The
//! backup uses the SQLite **online backup API**, which reads through an
//! uncheckpointed WAL — committed-but-uncheckpointed rows are captured
//! without forcing a checkpoint on the source (proved by
//! `tests/backup_restore.rs`). The bundle layout:
//!
//! ```text
//! <dest>/
//!   db.sqlite      full consistent copy of <home>/omt.db
//!   files/<rel>    every Markdown file under tickets/, path-preserved
//!   manifest.json  { createdAt, homeId, dbSha256, files: [{path, sha256}] }
//! ```
//!
//! `manifest.json` is written LAST, atomically: a crash mid-backup leaves a
//! dest WITHOUT a readable manifest, which [`restore_home`] refuses — the
//! source home is never touched by any step.

use crate::clock::MillisClock;
use crate::files::{check_contained, sha256_hex_bytes};
use crate::{Problem, Result};
use omt_domain::error;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Bundle file names inside the destination directory.
pub const DB_BUNDLE_NAME: &str = "db.sqlite";
pub const FILES_DIR_NAME: &str = "files";
pub const MANIFEST_NAME: &str = "manifest.json";

/// One Markdown entry recorded in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestFile {
    /// Home-relative path (`tickets/...`).
    pub path: String,
    /// SHA-256 of the exact UTF-8 bytes.
    pub sha256: String,
}

/// `manifest.json`: everything needed to verify and rebuild the bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupManifest {
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "homeId")]
    pub home_id: Option<String>,
    #[serde(rename = "dbSha256")]
    pub db_sha256: String,
    pub files: Vec<ManifestFile>,
}

fn io(problem_context: &str, err: std::io::Error) -> Problem {
    Problem::new(error::IO, format!("{problem_context}: {err}"))
}

/// Recursively collect `.md` files under `dir`, returning home-relative paths.
fn walk_markdown(dir: &Path, rel_prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let child_rel = format!("{rel_prefix}/{}", name.to_string_lossy());
            walk_markdown(&path, &child_rel, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let name = entry.file_name();
            out.push(format!("{rel_prefix}/{}", name.to_string_lossy()));
        }
    }
}

/// Every Markdown file under `<home>/tickets`, sorted, home-relative.
pub fn list_home_markdown(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_markdown(&home.join("tickets"), "tickets", &mut out);
    out.sort();
    out
}

/// Back up one home into `dest`. Steps in order:
///
/// 1. open the source DB read-only-ish and run the online backup API into
///    `<dest>/db.sqlite` (captures uncheckpointed WAL content);
/// 2. copy every Markdown file into `<dest>/files/<home-rel-path>`;
/// 3. write `manifest.json` LAST with SHA-256 over the copied DB bytes and
///    each Markdown file — atomically (temp + rename).
///
/// A failure at any point leaves the destination without a valid manifest,
/// making the bundle unusable while the source stays untouched.
pub fn backup_home(home: &Path, dest: &Path, clock: &dyn MillisClock) -> Result<BackupManifest> {
    std::fs::create_dir_all(dest).map_err(|err| io("create backup dest", err))?;
    let files_dir = dest.join(FILES_DIR_NAME);
    std::fs::create_dir_all(&files_dir).map_err(|err| io("create bundle files dir", err))?;

    // 1. DB via the SQLite online backup API.
    let db_bundle_path = dest.join(DB_BUNDLE_NAME);
    let _ = std::fs::remove_file(&db_bundle_path);
    let source_db_path = home.join(crate::DB_FILE_NAME);
    let source =
        Connection::open_with_flags(&source_db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| Problem::new(error::IO, format!("open source db for backup: {err}")))?;
    {
        let mut bundle = Connection::open(&db_bundle_path)
            .map_err(|err| Problem::new(error::IO, format!("open bundle db: {err}")))?;
        {
            let backup = Backup::new(&source, &mut bundle)
                .map_err(|err| Problem::new(error::IO, format!("init sqlite backup: {err}")))?;
            // Step until done so a large page count completes deterministically.
            loop {
                match backup
                    .step(512)
                    .map_err(|err| Problem::new(error::IO, format!("sqlite backup step: {err}")))?
                {
                    StepResult::Done => break,
                    StepResult::More | StepResult::Busy => continue,
                    other => {
                        return Err(Problem::new(
                            error::IO,
                            format!("unexpected sqlite backup state: {other:?}"),
                        ))
                    }
                }
            }
        }
        bundle
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(|err| Problem::new(error::IO, format!("quiesce bundle wal: {err}")))?;
    }

    // 2. Markdown tree.
    let mut manifest_files: Vec<ManifestFile> = Vec::new();
    for rel_path in list_home_markdown(home) {
        check_contained(&rel_path)?;
        let source_file = home.join(&rel_path);
        let bytes =
            std::fs::read(&source_file).map_err(|err| io("read markdown for backup", err))?;
        let target = files_dir.join(&rel_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io("create bundle dir", err))?;
        }
        std::fs::write(&target, &bytes).map_err(|err| io("write bundle file", err))?;
        manifest_files.push(ManifestFile {
            path: rel_path,
            sha256: sha256_hex_bytes(&bytes),
        });
    }

    // 3. Manifest LAST (atomic): its presence marks the bundle complete.
    let db_bytes = std::fs::read(&db_bundle_path).map_err(|err| io("hash bundle db", err))?;
    let manifest = BackupManifest {
        created_at: crate::clock::iso_from_ms(clock.now_ms()),
        home_id: crate::store::get_home_id(&source).unwrap_or(None),
        db_sha256: sha256_hex_bytes(&db_bytes),
        files: manifest_files,
    };
    let manifest_text = serde_json::to_string_pretty(&manifest)
        .map_err(|err| Problem::new(error::IO, format!("serialize manifest: {err}")))?;
    let tmp = dest.join(".manifest.tmp");
    std::fs::write(&tmp, manifest_text.as_bytes()).map_err(|err| io("write manifest tmp", err))?;
    std::fs::rename(&tmp, dest.join(MANIFEST_NAME)).map_err(|err| io("promote manifest", err))?;

    Ok(manifest)
}

/// Read + fully verify a bundle manifest: JSON parses, DB hash matches, and
/// every recorded file exists with matching content. Returns the manifest.
pub fn verify_bundle(dest: &Path) -> Result<BackupManifest> {
    let manifest_path = dest.join(MANIFEST_NAME);
    let text = std::fs::read_to_string(&manifest_path).map_err(|err| {
        Problem::with_details(
            error::IO,
            "backup bundle unusable: no readable manifest (crash mid-backup?)",
            |d| {
                d.insert("dest".into(), dest.display().to_string().into());
                d.insert("error".into(), err.to_string().into());
            },
        )
    })?;
    let manifest: BackupManifest = serde_json::from_str(&text).map_err(|err| {
        Problem::with_details(error::IO, "backup bundle unusable: corrupt manifest", |d| {
            d.insert("error".into(), err.to_string().into());
        })
    })?;
    let db_bytes = std::fs::read(dest.join(DB_BUNDLE_NAME)).map_err(|err| {
        Problem::with_details(
            error::IO,
            "backup bundle unusable: missing db.sqlite",
            |d| {
                d.insert("error".into(), err.to_string().into());
            },
        )
    })?;
    let actual_db_hash = sha256_hex_bytes(&db_bytes);
    if actual_db_hash != manifest.db_sha256 {
        return Err(Problem::with_details(
            error::IO,
            "backup bundle corrupt: db.sqlite hash mismatch",
            |d| {
                d.insert("expected".into(), manifest.db_sha256.clone().into());
                d.insert("actual".into(), actual_db_hash.into());
            },
        ));
    }
    for file in &manifest.files {
        let bytes = std::fs::read(dest.join(FILES_DIR_NAME).join(&file.path)).map_err(|err| {
            Problem::with_details(error::IO, "backup bundle corrupt: file missing", |d| {
                d.insert("path".into(), file.path.clone().into());
                d.insert("error".into(), err.to_string().into());
            })
        })?;
        let hash = sha256_hex_bytes(&bytes);
        if hash != file.sha256 {
            return Err(Problem::with_details(
                error::IO,
                "backup bundle corrupt: file hash mismatch",
                |d| {
                    d.insert("path".into(), file.path.clone().into());
                    d.insert("expected".into(), file.sha256.clone().into());
                    d.insert("actual".into(), hash.into());
                },
            ));
        }
    }
    Ok(manifest)
}

/// Restore a verified bundle into `target` (which must not already contain a
/// database): rebuilds the Markdown tree from `files/`, installs the DB as
/// `<target>/omt.db`, then runs SQLite `integrity_check` on the restored
/// database. Verification failures abort before ANY file is written to the
/// target home.
pub fn restore_home(dest: &Path, target: &Path) -> Result<BackupManifest> {
    // Verify BEFORE touching the target.
    let manifest = verify_bundle(dest)?;

    if target.join(crate::DB_FILE_NAME).exists() {
        return Err(Problem::with_details(
            error::IO,
            "restore target already contains a database; refusing to overwrite",
            |d| {
                d.insert("target".into(), target.display().to_string().into());
            },
        ));
    }
    std::fs::create_dir_all(target).map_err(|err| io("create restore target", err))?;
    std::fs::create_dir_all(target.join("tickets"))
        .map_err(|err| io("create tickets root", err))?;

    // Rebuild the Markdown tree.
    for file in &manifest.files {
        check_contained(&file.path)?;
        let bytes = std::fs::read(dest.join(FILES_DIR_NAME).join(&file.path))
            .map_err(|err| io("read bundle file for restore", err))?;
        let restored = target.join(&file.path);
        if let Some(parent) = restored.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io("create restore dir", err))?;
        }
        std::fs::write(&restored, &bytes).map_err(|err| io("write restored file", err))?;
    }

    // Install the DB, quiescing WAL so the restored home is self-contained.
    let db_target = target.join(crate::DB_FILE_NAME);
    std::fs::copy(dest.join(DB_BUNDLE_NAME), &db_target)
        .map_err(|err| io("install restored db", err))?;
    let conn = Connection::open(&db_target)
        .map_err(|err| Problem::new(error::IO, format!("open restored db: {err}")))?;
    conn.pragma_update(None, "journal_mode", "DELETE")
        .map_err(|err| Problem::new(error::IO, format!("quiesce restored wal: {err}")))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|err| Problem::new(error::IO, format!("integrity_check: {err}")))?;
    if integrity != "ok" {
        return Err(Problem::with_details(
            error::IO,
            "restored database failed integrity_check",
            |d| {
                d.insert("result".into(), integrity.into());
            },
        ));
    }
    Ok(manifest)
}
