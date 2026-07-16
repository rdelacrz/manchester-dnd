#![no_main]

use libfuzzer_sys::fuzz_target;
use manchester_dnd_core::{AiGmProposal, ai_turn::TypedGmProposal};
use manchester_dnd_server::generation::fuzz_provider_responses;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    fuzz_provider_responses(data);
    if let Ok(value) = serde_json::from_slice::<TypedGmProposal>(data) {
        round_trip(&value);
    }
    if let Ok(value) = serde_json::from_slice::<AiGmProposal>(data) {
        round_trip(&value);
    }
});

fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let canonical = serde_json::to_vec(value).expect("accepted proposals serialize");
    let decoded =
        serde_json::from_slice::<T>(&canonical).expect("serialized accepted proposals decode");
    assert_eq!(&decoded, value);
}
