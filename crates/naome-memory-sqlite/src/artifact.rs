use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Result, StoreError};

pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactDigest(pub [u8; 32]);

impl ArtifactDigest {
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    pub fn from_slice(bytes: &[u8], field: &'static str) -> Result<Self> {
        let digest = <[u8; 32]>::try_from(bytes).map_err(|_| StoreError::InvalidDigestLength {
            field,
            actual: bytes.len(),
        })?;
        Ok(Self(digest))
    }
}

impl std::fmt::Display for ArtifactDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub digest: ArtifactDigest,
    pub byte_len: u64,
    pub relative_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcAction {
    WouldMark,
    Marked,
    WaitingForGrace,
    WouldDelete,
    Deleted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GcReport {
    pub dry_run: bool,
    pub as_of_us: u64,
    pub grace_period_us: u64,
    pub entries: Vec<GcEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GcEntry {
    pub digest: ArtifactDigest,
    pub byte_len: u64,
    pub action: GcAction,
}

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { root: root.into() };
        create_dir_all_durable(&store.root)?;
        create_dir_all_durable(&store.root.join("sha256"))?;
        create_dir_all_durable(&store.root.join(".tmp"))?;
        Ok(store)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ingest_bytes(&self, bytes: &[u8]) -> Result<ArtifactRecord> {
        self.ingest(std::io::Cursor::new(bytes))
    }

    pub fn ingest(&self, mut reader: impl Read) -> Result<ArtifactRecord> {
        let temporary_path = self.unique_temporary_path()?;
        let mut temporary = TemporaryArtifact::new(temporary_path.clone());
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)?;
        let mut hasher = Sha256::new();
        let mut byte_len = 0_u64;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];

        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read)
                .map_err(|_| StoreError::ArtifactTooLarge { actual: u64::MAX })?;
            byte_len = byte_len
                .checked_add(read_u64)
                .ok_or(StoreError::ArtifactTooLarge { actual: u64::MAX })?;
            if byte_len > MAX_ARTIFACT_BYTES {
                return Err(StoreError::ArtifactTooLarge { actual: byte_len });
            }
            hasher.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
        drop(output);

        let digest = ArtifactDigest(hasher.finalize().into());
        let destination = self.path_for(digest);
        let parent = destination
            .parent()
            .ok_or_else(|| StoreError::InvalidArtifactPath(destination.display().to_string()))?;
        create_dir_all_durable(parent)?;

        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                Self::verify_path(&destination, digest, byte_len)?;
                fs::remove_file(&temporary_path)?;
                temporary.disarm();
                sync_directory(temporary_path.parent().ok_or_else(|| {
                    StoreError::InvalidArtifactPath(temporary_path.display().to_string())
                })?)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::rename(&temporary_path, &destination)?;
                temporary.disarm();
                // `rename` crosses directories. Both the destination entry and
                // removal of the staging entry must reach stable storage.
                sync_directory(parent)?;
                sync_directory(temporary_path.parent().ok_or_else(|| {
                    StoreError::InvalidArtifactPath(temporary_path.display().to_string())
                })?)?;
            }
            Err(error) => return Err(error.into()),
        }

        Ok(ArtifactRecord {
            digest,
            byte_len,
            relative_path: Self::relative_path_for(digest),
        })
    }

    pub fn verify(&self, record: &ArtifactRecord) -> Result<()> {
        Self::validate_record_metadata(record)?;
        Self::verify_path(
            &self.root.join(&record.relative_path),
            record.digest,
            record.byte_len,
        )
    }

    pub fn read_verified(&self, record: &ArtifactRecord) -> Result<Vec<u8>> {
        self.verify(record)?;
        let capacity =
            usize::try_from(record.byte_len).map_err(|_| StoreError::ArtifactTooLarge {
                actual: record.byte_len,
            })?;
        let mut bytes = Vec::with_capacity(capacity);
        File::open(self.root.join(&record.relative_path))?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Remove a GC-eligible CAS file after verifying its content. A missing
    /// file is accepted only for GC crash recovery: the preceding attempt may
    /// have durably unlinked the file before `SQLite` committed its metadata
    /// deletion.
    pub(crate) fn remove_verified_or_missing(&self, record: &ArtifactRecord) -> Result<()> {
        Self::validate_record_metadata(record)?;
        let path = self.root.join(&record.relative_path);
        match fs::symlink_metadata(&path) {
            Ok(_) => Self::verify_path(&path, record.digest, record.byte_len)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        fs::remove_file(&path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    /// Enumerate and verify every finalized CAS object, including objects that
    /// have no `SQLite` row because a process stopped after the durable rename.
    pub(crate) fn discover_records(&self) -> Result<Vec<ArtifactRecord>> {
        let mut records = Vec::new();
        let sha256_root = self.root.join("sha256");
        for first_entry in fs::read_dir(&sha256_root)? {
            let first_entry = first_entry?;
            require_directory_entry(&first_entry)?;
            let first = lowercase_hex_component(&first_entry.file_name(), 2)?;
            for second_entry in fs::read_dir(first_entry.path())? {
                let second_entry = second_entry?;
                require_directory_entry(&second_entry)?;
                let second = lowercase_hex_component(&second_entry.file_name(), 2)?;
                for object_entry in fs::read_dir(second_entry.path())? {
                    let object_entry = object_entry?;
                    require_file_entry(&object_entry)?;
                    let object = lowercase_hex_component(&object_entry.file_name(), 64)?;
                    if !object.starts_with(&first) || object[2..4] != second {
                        return Err(StoreError::InvalidArtifactPath(
                            object_entry.path().display().to_string(),
                        ));
                    }
                    let digest_bytes = hex::decode(&object).map_err(|_| {
                        StoreError::InvalidArtifactPath(object_entry.path().display().to_string())
                    })?;
                    let digest = ArtifactDigest::from_slice(&digest_bytes, "CAS file name")?;
                    let byte_len = object_entry.metadata()?.len();
                    if byte_len > MAX_ARTIFACT_BYTES {
                        return Err(StoreError::ArtifactTooLarge { actual: byte_len });
                    }
                    let record = ArtifactRecord {
                        digest,
                        byte_len,
                        relative_path: Self::relative_path_for(digest),
                    };
                    Self::verify_path(&object_entry.path(), digest, byte_len)?;
                    records.push(record);
                }
            }
        }
        records.sort_unstable_by_key(|record| record.digest);
        Ok(records)
    }

    pub(crate) fn validate_record_metadata(record: &ArtifactRecord) -> Result<()> {
        if record.byte_len > MAX_ARTIFACT_BYTES {
            return Err(StoreError::ArtifactTooLarge {
                actual: record.byte_len,
            });
        }
        let expected_relative = Self::relative_path_for(record.digest);
        if record.relative_path != expected_relative
            || !is_safe_relative_path(&record.relative_path)
        {
            return Err(StoreError::InvalidArtifactPath(
                record.relative_path.clone(),
            ));
        }
        Ok(())
    }

    fn verify_path(path: &Path, digest: ArtifactDigest, byte_len: u64) -> Result<()> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => {
                return Err(StoreError::ArtifactDigestMismatch {
                    path: path.to_path_buf(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::ArtifactMissing {
                    path: path.to_path_buf(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.len() != byte_len {
            return Err(StoreError::ArtifactDigestMismatch {
                path: path.to_path_buf(),
            });
        }
        let actual = hash_file(path, byte_len)?;
        if actual != digest {
            return Err(StoreError::ArtifactDigestMismatch {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    fn unique_temporary_path(&self) -> Result<PathBuf> {
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .root
                .join(".tmp")
                .join(format!("ingest-{}-{sequence}.partial", std::process::id()));
            if !path.exists() {
                return Ok(path);
            }
        }
        Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique artifact staging path",
        )))
    }

    fn path_for(&self, digest: ArtifactDigest) -> PathBuf {
        self.root.join(Self::relative_path_for(digest))
    }

    fn relative_path_for(digest: ArtifactDigest) -> String {
        let hex = digest.to_hex();
        format!("sha256/{}/{}/{}", &hex[..2], &hex[2..4], hex)
    }
}

fn hash_file(path: &Path, declared_len: u64) -> Result<ArtifactDigest> {
    if declared_len > MAX_ARTIFACT_BYTES {
        return Err(StoreError::ArtifactTooLarge {
            actual: declared_len,
        });
    }
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let read_u64 =
            u64::try_from(read).map_err(|_| StoreError::ArtifactTooLarge { actual: u64::MAX })?;
        observed = observed
            .checked_add(read_u64)
            .ok_or(StoreError::ArtifactTooLarge { actual: u64::MAX })?;
        if observed > MAX_ARTIFACT_BYTES {
            return Err(StoreError::ArtifactTooLarge { actual: observed });
        }
        hasher.update(&buffer[..read]);
    }
    if observed != declared_len {
        return Err(StoreError::ArtifactDigestMismatch {
            path: path.to_path_buf(),
        });
    }
    Ok(ArtifactDigest(hasher.finalize().into()))
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn create_dir_all_durable(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "artifact directory path is not a directory: {}",
                    path.display()
                ),
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent != path {
        create_dir_all_durable(parent)?;
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(path)?.file_type().is_dir() {
                return Err(error.into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    sync_directory(parent)
}

fn lowercase_hex_component(name: &std::ffi::OsStr, expected_len: usize) -> Result<String> {
    let value = name
        .to_str()
        .ok_or_else(|| StoreError::InvalidArtifactPath(name.to_string_lossy().into_owned()))?;
    if value.len() != expected_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StoreError::InvalidArtifactPath(value.to_owned()));
    }
    Ok(value.to_owned())
}

fn require_directory_entry(entry: &fs::DirEntry) -> Result<()> {
    if !entry.file_type()?.is_dir() {
        return Err(StoreError::InvalidArtifactPath(
            entry.path().display().to_string(),
        ));
    }
    Ok(())
}

fn require_file_entry(entry: &fs::DirEntry) -> Result<()> {
    if !entry.file_type()?.is_file() {
        return Err(StoreError::InvalidArtifactPath(
            entry.path().display().to_string(),
        ));
    }
    Ok(())
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

struct TemporaryArtifact {
    path: PathBuf,
    armed: bool,
}

impl TemporaryArtifact {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryArtifact {
    fn drop(&mut self) {
        if self.armed
            && fs::remove_file(&self.path).is_ok()
            && let Some(parent) = self.path.parent()
        {
            let _ignored = sync_directory(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_content_addressed_and_deduplicated() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("artifact store");
        let first = store.ingest_bytes(b"stable bytes").expect("first ingest");
        let second = store.ingest_bytes(b"stable bytes").expect("second ingest");

        assert_eq!(first, second);
        assert_eq!(
            store.read_verified(&first).expect("verified read"),
            b"stable bytes"
        );
    }

    #[test]
    fn oversize_stream_fails_and_leaves_no_partial_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("artifact store");
        let reader = std::io::repeat(0).take(MAX_ARTIFACT_BYTES + 1);

        assert!(matches!(
            store.ingest(reader),
            Err(StoreError::ArtifactTooLarge { .. })
        ));
        assert_eq!(
            fs::read_dir(directory.path().join(".tmp"))
                .expect("temporary directory exists")
                .count(),
            0
        );
    }

    #[test]
    fn tampering_is_detected() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("artifact store");
        let record = store.ingest_bytes(b"original").expect("ingest");
        fs::write(directory.path().join(&record.relative_path), b"tampered")
            .expect("tamper artifact");

        assert!(matches!(
            store.verify(&record),
            Err(StoreError::ArtifactDigestMismatch { .. })
        ));
    }

    #[test]
    fn finalized_objects_are_discovered_from_canonical_paths() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("artifact store");
        let second = store.ingest_bytes(b"second").expect("second ingest");
        let first = store.ingest_bytes(b"first").expect("first ingest");

        let mut expected = vec![first, second];
        expected.sort_unstable_by_key(|record| record.digest);
        assert_eq!(store.discover_records().expect("discover CAS"), expected);
        assert_eq!(
            fs::read_dir(directory.path().join(".tmp"))
                .expect("staging directory")
                .count(),
            0
        );
    }

    #[test]
    fn discovery_rejects_noncanonical_cas_entries() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("artifact store");
        fs::write(directory.path().join("sha256").join("unexpected"), b"bytes")
            .expect("write malformed CAS entry");

        assert!(matches!(
            store.discover_records(),
            Err(StoreError::InvalidArtifactPath(_))
        ));
    }
}
