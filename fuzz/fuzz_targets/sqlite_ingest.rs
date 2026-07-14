#![no_main]
#![forbid(unsafe_code)]

use libfuzzer_sys::fuzz_target;
use naome_memory_core::Digest32;
use naome_memory_sqlite::{DraftEvent, SqliteRepository, StoreConfig};

fuzz_target!(|data: &[u8]| {
    let bounded = data.get(..data.len().min(8 * 1024)).unwrap_or(data);
    if let Ok(directory) = tempfile::tempdir()
        && let Ok(mut repository) = SqliteRepository::open(&StoreConfig::new(
            directory.path().join("memory.db"),
            directory.path().join("artifacts"),
        ))
    {
        let digest = Digest32::hash_prefixed(&[], bounded);
        let draft_id = format!("fuzz-{}", digest.to_hex());
        if repository
            .create_draft(
                &draft_id,
                "fuzz-space",
                Some("fuzz-repository"),
                Some("fuzz-task"),
                Some("fuzz-agent"),
                "fuzz-session",
                1,
            )
            .is_ok()
        {
            let event = DraftEvent {
                sequence: 0,
                event_at_us: 2,
                event_kind: "fuzz-payload".to_owned(),
                canonical_payload: bounded.to_vec(),
                payload_digest: digest.0,
            };
            let first = repository.append_event(&draft_id, &event);
            let loaded = repository.load_events(&draft_id);
            assert_eq!(first.is_ok(), loaded.is_ok());
            if let Ok(events) = loaded {
                assert_eq!(events, vec![event]);
                assert!(repository.check_integrity(false).is_ok());
            }
        }
    }
});
