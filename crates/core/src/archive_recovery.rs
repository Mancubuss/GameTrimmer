//! Strict, read-only contract model for a future archive recovery sidecar.
//!
//! This module deliberately performs no filesystem writes and exposes no
//! mutation-ready token. Parsing validates only the persisted contract shape;
//! it is not proof that a backup is durable, current, or safe to restore.
//! A future executor must add same-handle live identity/hash/link-count checks,
//! durable publication, reconciliation, and verified rollback before archive
//! mutation can be enabled.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::safety::{FileIdentity, TargetKind};

const MANIFEST_VERSION: u32 = 1;
const SHA256_HEX_LENGTH: usize = 64;

#[derive(Debug, Error)]
enum RecoveryContractError {
    #[error("recovery manifest JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid recovery manifest: {0}")]
    Invalid(String),
}

/// A declaration of complete backup material. This is intentionally not named
/// `Ready`: a structurally valid declaration still needs live and durable I/O
/// verification before any destructive caller may trust it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryBackupManifestV1 {
    version: u32,
    operation_nonce: String,
    archive_format: String,
    parser_version: String,
    trusted_root: String,
    relative_path: String,
    root_identity: String,
    target_path: String,
    target_identity: String,
    target_size: u64,
    target_sha256: String,
    payload_length: u64,
    payload_sha256: String,
    extents: Vec<RecoveryExtentV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryExtentV1 {
    target_offset: u64,
    length: u64,
    payload_offset: u64,
    preimage_sha256: String,
}

/// Opaque proof that the JSON contract itself is canonical and internally
/// consistent. It conveys no live-filesystem or durability guarantee.
#[derive(Debug)]
struct ValidatedBackupContract(RecoveryBackupManifestV1);

fn parse_backup_contract(bytes: &[u8]) -> Result<ValidatedBackupContract, RecoveryContractError> {
    let manifest: RecoveryBackupManifestV1 = serde_json::from_slice(bytes)?;
    validate_backup_contract(&manifest)?;
    Ok(ValidatedBackupContract(manifest))
}

fn validate_backup_contract(
    manifest: &RecoveryBackupManifestV1,
) -> Result<(), RecoveryContractError> {
    if manifest.version != MANIFEST_VERSION {
        return invalid(format!("unsupported manifest version {}", manifest.version));
    }

    let parsed_nonce = uuid::Uuid::parse_str(&manifest.operation_nonce)
        .map_err(|_| RecoveryContractError::Invalid("operation nonce is not a UUID".into()))?;
    if parsed_nonce.to_string() != manifest.operation_nonce {
        return invalid("operation nonce is not in canonical lowercase form");
    }
    validate_label("archive format", &manifest.archive_format)?;
    validate_label("parser version", &manifest.parser_version)?;

    let trusted_root = Path::new(&manifest.trusted_root);
    let target_path = Path::new(&manifest.target_path);
    let relative_path = crate::safety::normalize_relative_path(&manifest.relative_path)
        .map_err(|error| RecoveryContractError::Invalid(error.to_string()))?;
    if !trusted_root.is_absolute() || !target_path.is_absolute() {
        return invalid("trusted root and target path must be absolute");
    }
    if relative_path != Path::new(&manifest.relative_path) {
        return invalid("relative path is not in canonical normalized form");
    }
    if trusted_root.join(&relative_path) != target_path {
        return invalid("target path is not the trusted root joined with relative path");
    }

    let root_identity = decode_identity("trusted root", &manifest.root_identity)?;
    if root_identity.kind != TargetKind::Directory {
        return invalid("trusted root identity is not a directory");
    }
    let target_identity = decode_identity("target", &manifest.target_identity)?;
    if target_identity.kind != TargetKind::File || target_identity.size != manifest.target_size {
        return invalid("target identity does not match target file size/type");
    }
    if manifest.target_size == 0 {
        return invalid("target archive is empty");
    }

    validate_sha256("target", &manifest.target_sha256)?;
    validate_sha256("payload", &manifest.payload_sha256)?;
    if manifest.extents.is_empty() {
        return invalid("no recovery extents were declared");
    }

    let mut previous_target_end = 0u64;
    let mut next_payload_offset = 0u64;
    for (index, extent) in manifest.extents.iter().enumerate() {
        if extent.length == 0 {
            return invalid(format!("extent #{index} is empty"));
        }
        let target_end = extent
            .target_offset
            .checked_add(extent.length)
            .ok_or_else(|| RecoveryContractError::Invalid(format!("extent #{index} overflows")))?;
        if target_end > manifest.target_size
            || (index > 0 && extent.target_offset <= previous_target_end)
        {
            return invalid(format!(
                "extent #{index} is out of bounds, unordered, or overlapping"
            ));
        }
        if extent.payload_offset != next_payload_offset {
            return invalid(format!("extent #{index} payload is not contiguous"));
        }
        validate_sha256(
            &format!("extent #{index} preimage"),
            &extent.preimage_sha256,
        )?;
        previous_target_end = target_end;
        next_payload_offset = next_payload_offset
            .checked_add(extent.length)
            .ok_or_else(|| {
                RecoveryContractError::Invalid("backup payload length overflows".into())
            })?;
    }
    if next_payload_offset != manifest.payload_length {
        return invalid("payload length does not equal the declared extents");
    }
    Ok(())
}

fn validate_label(name: &str, value: &str) -> Result<(), RecoveryContractError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'/' || byte == b'\\')
    {
        return invalid(format!("{name} is empty or malformed"));
    }
    Ok(())
}

fn decode_identity(name: &str, encoded: &str) -> Result<FileIdentity, RecoveryContractError> {
    FileIdentity::decode(encoded)
        .map_err(|error| RecoveryContractError::Invalid(format!("{name} identity: {error}")))
}

fn validate_sha256(name: &str, value: &str) -> Result<(), RecoveryContractError> {
    if value.len() != SHA256_HEX_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid(format!("{name} SHA-256 is malformed"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, RecoveryContractError> {
    Err(RecoveryContractError::Invalid(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn identity(kind: TargetKind, size: u64, index: u64) -> String {
        FileIdentity {
            volume_serial: 7,
            file_index: index,
            kind,
            size,
            last_write_time: 99,
            attributes: 0,
        }
        .encode()
    }

    fn fixture(root: &Path) -> Value {
        let target_size = 4096;
        let target = root.join("data").join("voices.pck");
        json!({
            "version": 1,
            "operation_nonce": "123e4567-e89b-42d3-a456-426614174000",
            "archive_format": "wwise_pck",
            "parser_version": "archive_trimmer-1",
            "trusted_root": root.to_string_lossy(),
            "relative_path": PathBuf::from("data").join("voices.pck").to_string_lossy(),
            "root_identity": identity(TargetKind::Directory, 0, 1),
            "target_path": target.to_string_lossy(),
            "target_identity": identity(TargetKind::File, target_size, 2),
            "target_size": target_size,
            "target_sha256": "a".repeat(64),
            "payload_length": 96,
            "payload_sha256": "b".repeat(64),
            "extents": [
                {
                    "target_offset": 128,
                    "length": 32,
                    "payload_offset": 0,
                    "preimage_sha256": "c".repeat(64)
                },
                {
                    "target_offset": 1024,
                    "length": 64,
                    "payload_offset": 32,
                    "preimage_sha256": "d".repeat(64)
                }
            ]
        })
    }

    fn parse(value: &Value) -> Result<ValidatedBackupContract, RecoveryContractError> {
        parse_backup_contract(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn valid_contract_binds_root_target_parser_and_exact_extents() {
        let root = tempfile::tempdir().unwrap().path().to_path_buf();
        let contract = parse(&fixture(&root)).unwrap();

        assert_eq!(contract.0.archive_format, "wwise_pck");
        assert_eq!(contract.0.parser_version, "archive_trimmer-1");
        assert_eq!(contract.0.extents.len(), 2);
        assert_eq!(contract.0.extents[1].payload_offset, 32);
    }

    #[test]
    fn schema_rejects_unknown_missing_or_defaulted_fields() {
        let root = tempfile::tempdir().unwrap().path().to_path_buf();
        let mut unknown = fixture(&root);
        unknown["unexpected"] = json!(true);
        assert!(matches!(
            parse(&unknown),
            Err(RecoveryContractError::Json(_))
        ));

        let mut missing = fixture(&root);
        missing.as_object_mut().unwrap().remove("payload_sha256");
        assert!(matches!(
            parse(&missing),
            Err(RecoveryContractError::Json(_))
        ));

        let mut extent_unknown = fixture(&root);
        extent_unknown["extents"][0]["future_default"] = json!(0);
        assert!(matches!(
            parse(&extent_unknown),
            Err(RecoveryContractError::Json(_))
        ));
    }

    #[test]
    fn malformed_or_mismatched_binding_fails_closed() {
        let root = tempfile::tempdir().unwrap().path().to_path_buf();
        for (field, value) in [
            ("version", json!(2)),
            ("operation_nonce", json!("not-a-uuid")),
            ("target_sha256", json!("CRC32-is-not-enough")),
            ("relative_path", json!("../voices.pck")),
            ("target_size", json!(4095)),
        ] {
            let mut manifest = fixture(&root);
            manifest[field] = value;
            assert!(matches!(
                parse(&manifest),
                Err(RecoveryContractError::Invalid(_))
            ));
        }

        let mut smuggled_target = fixture(&root);
        smuggled_target["target_path"] = json!(root.join("manual.txt").to_string_lossy());
        assert!(matches!(
            parse(&smuggled_target),
            Err(RecoveryContractError::Invalid(_))
        ));
    }

    #[test]
    fn windows_ambiguous_relative_paths_fail_closed() {
        let root = tempfile::tempdir().unwrap().path().to_path_buf();
        for relative_path in [
            "voices.pck.",
            "voices.pck ",
            "voices.pck:stream",
            "voices\0.pck",
            "../voices.pck",
            "NUL.pck",
            "data/COM1.bin",
            "data/COM¹.bin",
            "data/LPT².pck",
            "CONIN$",
            "CONOUT$.pck",
        ] {
            let mut manifest = fixture(&root);
            manifest["relative_path"] = json!(relative_path);
            manifest["target_path"] = json!(root.join(relative_path).to_string_lossy());
            assert!(
                matches!(parse(&manifest), Err(RecoveryContractError::Invalid(_))),
                "accepted ambiguous path {relative_path:?}"
            );
        }
    }

    #[test]
    fn extents_must_be_canonical_bounded_and_payload_complete() {
        let root = tempfile::tempdir().unwrap().path().to_path_buf();
        let cases = [("extents", json!([])), ("payload_length", json!(95))];
        for (field, value) in cases {
            let mut manifest = fixture(&root);
            manifest[field] = value;
            assert!(matches!(
                parse(&manifest),
                Err(RecoveryContractError::Invalid(_))
            ));
        }

        for extents in [
            json!([{
                "target_offset": 0,
                "length": 0,
                "payload_offset": 0,
                "preimage_sha256": "c".repeat(64)
            }]),
            json!([{
                "target_offset": 4080,
                "length": 32,
                "payload_offset": 0,
                "preimage_sha256": "c".repeat(64)
            }]),
            json!([
                {
                    "target_offset": 128,
                    "length": 64,
                    "payload_offset": 0,
                    "preimage_sha256": "c".repeat(64)
                },
                {
                    "target_offset": 160,
                    "length": 32,
                    "payload_offset": 64,
                    "preimage_sha256": "d".repeat(64)
                }
            ]),
            json!([
                {
                    "target_offset": 128,
                    "length": 32,
                    "payload_offset": 0,
                    "preimage_sha256": "c".repeat(64)
                },
                {
                    "target_offset": 160,
                    "length": 32,
                    "payload_offset": 32,
                    "preimage_sha256": "d".repeat(64)
                }
            ]),
        ] {
            let mut manifest = fixture(&root);
            manifest["extents"] = extents;
            assert!(matches!(
                parse(&manifest),
                Err(RecoveryContractError::Invalid(_))
            ));
        }
    }
}
