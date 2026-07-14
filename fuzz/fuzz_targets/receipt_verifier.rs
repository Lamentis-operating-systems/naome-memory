#![no_main]
#![forbid(unsafe_code)]

use libfuzzer_sys::fuzz_target;
use naome_memory_core::{ProofReceiptV1, ReceiptVerificationInputsV1, verify_receipt};

fuzz_target!(|data: &[u8]| {
    if let Ok((receipt, inputs)) =
        serde_json::from_slice::<(ProofReceiptV1, ReceiptVerificationInputsV1)>(data)
    {
        let first = verify_receipt(&receipt, &inputs);
        let second = verify_receipt(&receipt, &inputs);
        assert_eq!(first, second);
    }
});
