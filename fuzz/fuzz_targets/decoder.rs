#![no_main]
#![forbid(unsafe_code)]

use libfuzzer_sys::fuzz_target;
use naome_memory_core::{CanonicalBytes as _, MemoryAtomBodyV1};

fuzz_target!(|data: &[u8]| {
    if let Ok(body) = serde_json::from_slice::<MemoryAtomBodyV1>(data) {
        let first = body.canonical_bytes();
        let second = body.canonical_bytes();
        assert_eq!(first, second);
        if let Ok(atom_id) = body.atom_id() {
            assert_eq!(body.atom_id(), Ok(atom_id));
        }
    }
});
