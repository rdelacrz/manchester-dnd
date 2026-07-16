#![no_main]

use libfuzzer_sys::fuzz_target;
use manchester_dnd_core::{
    AdvanceNpcTurnCommand, AttemptExplorationCheckCommand, AttemptSocialInteractionCommand,
    CommitEncounterCommand, rules_matrix::D20TestRequest,
};
use manchester_dnd_server::{
    CampaignLifecycleCommand, ImageBrief, inspiration::ConfigureCampaignInspirationCommand,
};

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    if let Ok(value) = serde_json::from_slice::<D20TestRequest>(data) {
        let _ = value.validate();
    }
    if let Ok(value) = serde_json::from_slice::<AttemptExplorationCheckCommand>(data) {
        let _ = value.validate();
    }
    if let Ok(value) = serde_json::from_slice::<AttemptSocialInteractionCommand>(data) {
        let _ = value.validate();
    }
    if let Ok(value) = serde_json::from_slice::<CommitEncounterCommand>(data) {
        let _ = value.validate();
    }
    if let Ok(value) = serde_json::from_slice::<AdvanceNpcTurnCommand>(data) {
        let _ = value.validate();
    }
    if let Ok(value) = serde_json::from_slice::<CampaignLifecycleCommand>(data) {
        let _ = value.validate();
    }
    if let Ok(value) = serde_json::from_slice::<ImageBrief>(data) {
        let _ = value.validate();
    }
    let _ = serde_json::from_slice::<ConfigureCampaignInspirationCommand>(data);
});
