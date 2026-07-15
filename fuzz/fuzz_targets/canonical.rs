#![no_main]
#![forbid(unsafe_code)]

use libfuzzer_sys::fuzz_target;
use naome_memory_core::canonical::canonical_binary;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
        let first = canonical_binary(&value);
        let second = canonical_binary(&value);
        assert_eq!(first, second);
    }
});
