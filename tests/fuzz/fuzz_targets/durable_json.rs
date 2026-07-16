#![no_main]

use libfuzzer_sys::fuzz_target;
use manchester_dnd_core::{D20Roll, RollRecord, SessionDto, SessionEventDto};
use manchester_dnd_server::CampaignPrivateExportV1;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }

    if let Ok(value) = serde_json::from_slice::<SessionDto>(data) {
        let _ = value.validate();
        round_trip(&value);
    }
    if let Ok(value) = serde_json::from_slice::<SessionEventDto>(data) {
        let _ = value.validate();
        round_trip(&value);
    }
    if let Ok(value) = serde_json::from_slice::<RollRecord>(data) {
        let _ = value.validate();
        round_trip(&value);
    }
    if let Ok(value) = serde_json::from_slice::<D20Roll>(data) {
        let _ = value.validate();
        round_trip(&value);
    }
    if let Ok(value) = serde_json::from_slice::<CampaignPrivateExportV1>(data) {
        let _ = value.validate();
        round_trip(&value);
    }
});

fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let canonical = serde_json::to_vec(value).expect("accepted durable values serialize");
    let decoded =
        serde_json::from_slice::<T>(&canonical).expect("serialized accepted durable values decode");
    assert_eq!(&decoded, value);
}
