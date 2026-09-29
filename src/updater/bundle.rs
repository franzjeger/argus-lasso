//! Matched app/layer installation with durable recovery and one-step rollback.
//! Libraries are written to unique paths and never overwrite mapped files.
use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Metadata {
    pub schema: u32,
    pub version: String,
    pub build_id: String,
    pub protocol: u32,
    pub arch: String,
    pub app_sha256: String,
    pub layer_sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Rollback {
    target: PathBuf,
    binary: PathBuf,
    binary_sha256: String,
    manifest_path: PathBuf,
    manifest: Option<Vec<u8>>,
    pending: bool,
}
fn journal_path(target: &Path) -> Result<PathBuf, String> {
    Ok(target
        .parent()
        .ok_or("binary has no parent")?
        .join(".argus-lasso-rollback.json"))
}
fn write_journal(path: &Path, rollback: &Rollback) -> Result<(), String> {
    crate::config::atomic_write(
        path,
        &serde_json::to_vec(rollback).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let stage = path.with_file_name(format!(".argus-executable-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        crate::config::atomic_write(&stage, bytes)?;
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o755))?;
        std::fs::File::open(&stage)?.sync_all()?;
        std::fs::rename(&stage, path)?;
        std::fs::File::open(path.parent().unwrap())?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&stage);
    }
    result.map_err(|e| format!("Could not install {}: {e}", path.display()))
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}
fn restore(record: &Rollback) -> Result<(), String> {
    let binary =
        std::fs::read(&record.binary).map_err(|e| format!("Rollback binary unavailable: {e}"))?;
    if sha256_hex(&binary) != record.binary_sha256 {
        return Err("Rollback binary checksum mismatch".into());
    }
    // Keep the record until BOTH restores succeed; a retry is idempotent.
    write_executable(&record.target, &binary)?;
    if let Some(manifest) = &record.manifest {
        crate::config::atomic_write(&record.manifest_path, manifest).map_err(|e| e.to_string())?;
    } else {
        match std::fs::remove_file(&record.manifest_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        std::fs::File::open(record.manifest_path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub(super) fn rollback(target: &Path, pending_only: bool) -> Result<bool, String> {
    let journal = journal_path(target)?;
    let Some(bytes) = read_optional(&journal)? else {
        return Ok(false);
    };
    let mut record: Rollback =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid rollback record: {e}"))?;
    if record.target != target {
        return Err("Rollback record belongs to a different installation".into());
    }
    if pending_only && !record.pending {
        return Ok(false);
    }
    record.pending = true;
    write_journal(&journal, &record)?;
    restore(&record)?;
    std::fs::remove_file(&journal).map_err(|e| e.to_string())?;
    std::fs::File::open(journal.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(true)
}
pub(super) fn available(target: &Path) -> bool {
    journal_path(target).is_ok_and(|p| p.is_file())
}

pub(super) fn install(tarball: &[u8], target: &Path, expected_version: &str) -> Result<(), String> {
    let app = extract_binary(tarball)?;
    let layer = extract_exact(tarball, "*/libargus_layer.so")?;
    let metadata: Metadata = serde_json::from_slice(&extract_exact(tarball, "*/bundle.json")?)
        .map_err(|e| format!("Release has no valid matched-bundle metadata: {e}"))?;
    validate_metadata(&metadata, &app, &layer, expected_version)?;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?);
    let manifest_path = home.join(".local/share/vulkan/implicit_layer.d/ArgusOverlay.json");
    install_pair(
        target,
        &manifest_path,
        &home.join(".local/share/argus-lasso/layers"),
        &app,
        &layer,
        &metadata,
        |stage| {
            verify_staged_binary_is_an_upgrade(stage)?;
            let out = std::process::Command::new(stage)
                .arg("build-info")
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("Staged app cannot report bundle compatibility".into());
            }
            let info: serde_json::Value =
                serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
            if info["version"] != metadata.version
                || info["build_id"] != metadata.build_id
                || info["protocol"] != metadata.protocol
                || info["arch"] != metadata.arch
            {
                return Err("App identity does not match the signed bundle metadata".into());
            }
            Ok(())
        },
    )
}
fn validate_metadata(
    metadata: &Metadata,
    app: &[u8],
    layer: &[u8],
    version: &str,
) -> Result<(), String> {
    if metadata.schema != 1
        || metadata.protocol == 0
        || metadata.version != version
        || metadata.arch != std::env::consts::ARCH
        || metadata.build_id.is_empty()
        || metadata.app_sha256 != sha256_hex(app)
        || metadata.layer_sha256 != sha256_hex(layer)
    {
        return Err(
            "Release app/layer compatibility or checksum mismatch; nothing installed".into(),
        );
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn install_pair(
    target: &Path,
    manifest_path: &Path,
    layers: &Path,
    app: &[u8],
    layer: &[u8],
    metadata: &Metadata,
    verify: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let _guard = crate::gui::overlay_install::MANIFEST_LOCK
        .lock()
        .map_err(|_| "Overlay update lock failed")?;
    let result = install_pair_locked(target, manifest_path, layers, app, layer, metadata, verify);
    prune_stale_updates(target, manifest_path, layers);
    result
}

/// Remove update directories nothing refers to any more.
///
/// Each install leaves a transaction directory next to the app (holding the
/// previous app for rollback) and a layer directory. The next install
/// replaces the only rollback record, and a failed one never records its
/// directories at all, so without this every update left files behind for
/// good. Kept: the live manifest's layer, and the app and layer the rollback
/// record would restore. Anything that cannot be read with certainty is kept.
fn prune_stale_updates(target: &Path, manifest_path: &Path, layers: &Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(journal) = journal_path(target) else {
        return;
    };
    let record: Option<Rollback> = match read_optional(&journal) {
        Ok(None) => None,
        Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
            Ok(record) => Some(record),
            Err(_) => return,
        },
        Err(_) => return,
    };
    let manifest = match read_optional(manifest_path) {
        Ok(manifest) => manifest,
        Err(_) => return,
    };
    let layer_dir_of = |manifest: &[u8]| -> Option<PathBuf> {
        let value: serde_json::Value = serde_json::from_slice(manifest).ok()?;
        Some(
            Path::new(value["layer"]["library_path"].as_str()?)
                .parent()?
                .into(),
        )
    };
    if manifest.is_some() && manifest.as_deref().and_then(layer_dir_of).is_none() {
        return;
    }
    // Compared by inode, not by spelling: a symlinked HOME must not make a
    // live directory look unreferenced.
    let keep: Vec<(u64, u64)> = [
        manifest.as_deref().and_then(layer_dir_of),
        record
            .as_ref()
            .and_then(|r| r.binary.parent().map(Path::to_path_buf)),
        record
            .as_ref()
            .and_then(|r| r.manifest.as_deref())
            .and_then(layer_dir_of),
    ]
    .into_iter()
    .flatten()
    .filter_map(|dir| std::fs::metadata(dir).ok())
    .map(|m| (m.dev(), m.ino()))
    .collect();
    let prune = |dir: &Path, prefix: &str| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let ours = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(prefix));
            // file_type() does not follow symlinks: only real directories.
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if ours && is_dir && !keep.contains(&(meta.dev(), meta.ino())) {
                if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                    log::warn!("Could not remove stale {}: {e}", entry.path().display());
                }
            }
        }
    };
    if let Some(bin_dir) = target.parent() {
        prune(bin_dir, ".argus-update-");
    }
    prune(layers, "update-");
}

#[allow(clippy::too_many_arguments)]
fn install_pair_locked(
    target: &Path,
    manifest_path: &Path,
    layers: &Path,
    app: &[u8],
    layer: &[u8],
    metadata: &Metadata,
    verify: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    // Finish recovery before replacing the only rollback record.
    rollback(target, true)?;
    let transaction = target
        .parent()
        .ok_or("missing binary directory")?
        .join(format!(".argus-update-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&transaction)
        .map_err(|e| e.to_string())?;
    let staged = transaction.join("argus-lasso");
    write_executable(&staged, app)?;
    verify(&staged)?; // No live files have changed yet.
    std::fs::create_dir_all(layers).map_err(|e| e.to_string())?;
    let layer_dir = layers.join(format!("update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&layer_dir).map_err(|e| e.to_string())?;
    let layer_path = layer_dir.join("libargus_layer.so");
    write_executable(&layer_path, layer)?;
    std::fs::File::open(layers)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(manifest_path.parent().unwrap()).map_err(|e| e.to_string())?;
    let old_manifest = read_optional(manifest_path)?;
    let mut manifest: serde_json::Value = match old_manifest.as_ref() {
        Some(bytes) => serde_json::from_slice(bytes)
            .map_err(|e| format!("Existing overlay manifest is invalid: {e}"))?,
        None => serde_json::json!({"file_format_version":"1.0.0", "layer": {
            "name":"VK_LAYER_ARGUS_OVERLAY", "type":"GLOBAL", "api_version":"1.3.200", "library_arch":"64",
            "enable_environment":{"ARGUS_LASSO_HUD":"1"}, "disable_environment":{"ARGUS_LASSO_HUD_DISABLE":"1"}
        }}),
    };
    if !manifest["layer"].is_object() || manifest["layer"]["name"] != "VK_LAYER_ARGUS_OVERLAY" {
        return Err("Existing manifest does not describe the Argus layer".into());
    }
    manifest["layer"]["library_path"] = layer_path.to_string_lossy().into_owned().into();
    manifest["layer"]["implementation_version"] = metadata.protocol.to_string().into();
    manifest["layer"]["description"] =
        format!("Argus-Lasso telemetry HUD (IPC v{})", metadata.protocol).into();
    let previous = std::fs::read(target).map_err(|e| e.to_string())?;
    let backup = transaction.join("previous-app");
    write_executable(&backup, &previous)?;
    let mut record = Rollback {
        target: target.into(),
        binary: backup,
        binary_sha256: sha256_hex(&previous),
        manifest_path: manifest_path.into(),
        manifest: old_manifest,
        pending: true,
    };
    let journal = journal_path(target)?;
    write_journal(&journal, &record)?;
    let result = (|| {
        crate::config::atomic_write(
            manifest_path,
            &serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&staged, target).map_err(|e| e.to_string())?;
        std::fs::File::open(target.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        record.pending = false;
        write_journal(&journal, &record)
    })();
    if let Err(error) = result {
        return match rollback(target, false) {
            Ok(_) => Err(format!(
                "Update failed; previous app/layer restored: {error}"
            )),
            Err(recovery) => Err(format!(
                "Update failed: {error}. Recovery needs retry: {recovery}"
            )),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata() -> Metadata {
        Metadata {
            schema: 1,
            version: "999.0.0".into(),
            build_id: "test".into(),
            protocol: 6,
            arch: std::env::consts::ARCH.into(),
            app_sha256: sha256_hex(b"new app"),
            layer_sha256: sha256_hex(b"new layer"),
        }
    }
    #[test]
    fn rejects_mismatched_pair_before_installation() {
        assert!(validate_metadata(&metadata(), b"new app", b"wrong layer", "999.0.0").is_err());
    }
    #[test]
    fn installs_matched_pair_preserves_loading_mode_and_rolls_back() {
        let root = std::env::temp_dir().join(format!("argus-pair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let app = root.join("app");
        let manifest = root.join("manifest.json");
        std::fs::write(&app, b"old app").unwrap();
        let old = br#"{"layer":{"name":"VK_LAYER_ARGUS_OVERLAY","library_path":"/old/layer.so"}}"#;
        std::fs::write(&manifest, old).unwrap();
        install_pair(
            &app,
            &manifest,
            &root.join("layers"),
            b"new app",
            b"new layer",
            &metadata(),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(std::fs::read(&app).unwrap(), b"new app");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        assert!(value["layer"].get("enable_environment").is_none());
        assert_eq!(
            std::fs::read(value["layer"]["library_path"].as_str().unwrap()).unwrap(),
            b"new layer"
        );
        assert!(!rollback(&app, true).unwrap());
        assert!(rollback(&app, false).unwrap());
        assert_eq!(std::fs::read(&app).unwrap(), b"old app");
        assert_eq!(std::fs::read(&manifest).unwrap(), old);
        std::fs::remove_dir_all(root).unwrap();
    }
    fn entries_with_prefix(dir: &Path, prefix: &str) -> usize {
        std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Each install replaces the only rollback record. What it no longer
    /// references must go, or every update leaves an app and a layer behind.
    #[test]
    fn repeated_updates_keep_only_what_rollback_needs() {
        let root = std::env::temp_dir().join(format!("argus-pair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let app = root.join("app");
        let manifest = root.join("manifest.json");
        let layers = root.join("layers");
        std::fs::write(&app, b"app 0").unwrap();
        for n in 1..=3 {
            let (new_app, new_layer) = (format!("app {n}"), format!("layer {n}"));
            let mut meta = metadata();
            meta.app_sha256 = sha256_hex(new_app.as_bytes());
            meta.layer_sha256 = sha256_hex(new_layer.as_bytes());
            install_pair(
                &app,
                &manifest,
                &layers,
                new_app.as_bytes(),
                new_layer.as_bytes(),
                &meta,
                |_| Ok(()),
            )
            .unwrap();
        }
        // The previous app for rollback, and the live plus previous layer.
        assert_eq!(entries_with_prefix(&root, ".argus-update-"), 1);
        assert_eq!(entries_with_prefix(&layers, "update-"), 2);

        assert!(rollback(&app, false).unwrap());
        assert_eq!(std::fs::read(&app).unwrap(), b"app 2");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(
            std::fs::read(value["layer"]["library_path"].as_str().unwrap()).unwrap(),
            b"layer 2",
            "the layer rollback restores must survive pruning"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_verification_leaves_live_files_untouched() {
        let root = std::env::temp_dir().join(format!("argus-pair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let app = root.join("app");
        let manifest = root.join("manifest.json");
        std::fs::write(&app, b"old app").unwrap();
        assert!(install_pair(
            &app,
            &manifest,
            &root.join("layers"),
            b"new app",
            b"new layer",
            &metadata(),
            |_| Err("identity mismatch".into())
        )
        .is_err());
        assert_eq!(std::fs::read(&app).unwrap(), b"old app");
        assert!(!manifest.exists());
        assert_eq!(
            entries_with_prefix(&root, ".argus-update-"),
            0,
            "a rejected update must not leave its staged app behind"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_install_recovers_on_next_start() {
        let root = std::env::temp_dir().join(format!("argus-pair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let app = root.join("app");
        let manifest = root.join("manifest.json");
        let backup = root.join("backup");
        std::fs::write(&app, b"new app").unwrap();
        std::fs::write(&backup, b"old app").unwrap();
        std::fs::write(&manifest, b"new manifest").unwrap();
        write_journal(
            &journal_path(&app).unwrap(),
            &Rollback {
                target: app.clone(),
                binary: backup,
                binary_sha256: sha256_hex(b"old app"),
                manifest_path: manifest.clone(),
                manifest: None,
                pending: true,
            },
        )
        .unwrap();
        assert!(rollback(&app, true).unwrap());
        assert_eq!(std::fs::read(&app).unwrap(), b"old app");
        assert!(!manifest.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
