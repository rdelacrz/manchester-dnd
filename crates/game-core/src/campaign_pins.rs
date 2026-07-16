//! Durable, immutable campaign provenance pins.
//!
//! A campaign is allowed to exist briefly as an unsealed character-creator
//! scaffold. Once a theme is selected, the exact rules, content, prompt, and
//! schema identities are sealed together and must never be reinterpreted.

use serde::{Deserialize, Serialize};

use crate::{
    GameCoreError, Result, SESSION_SCHEMA_VERSION, Sha256Digest,
    ai_turn::TYPED_AI_PROPOSAL_SCHEMA_VERSION,
    encounter::ENCOUNTER_SCHEMA_VERSION,
    hero::{HERO_CHARACTER_SCHEMA_VERSION, HERO_DRAFT_SCHEMA_VERSION, HeroPins},
    is_valid_opaque_id,
    rules_matrix::RULES_MATRIX_SCHEMA_VERSION,
};

pub const CAMPAIGN_PINS_SCHEMA_VERSION: u16 = 1;
pub const CONTENT_PACK_SCHEMA_ID: &str = "content-pack/v1";
pub const TYPED_GM_REQUEST_SCHEMA_ID: &str = "typed-gm-request/v1";
pub const CAMPAIGN_PROMPT_TEMPLATE_ID: &str = "prompt:typed-gm-turn:v1";
pub const CAMPAIGN_PROMPT_POLICY_ID: &str = "policy:private-mvp:v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPromptPin {
    pub template_id: String,
    pub template_digest: Sha256Digest,
    pub policy_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignSchemaPins {
    pub session: u16,
    pub hero_draft: u16,
    pub hero_character: u16,
    pub encounter: u16,
    pub rules_matrix: u16,
    pub typed_ai_proposal: u16,
    pub content_pack: String,
    pub typed_gm_request: String,
}

impl CampaignSchemaPins {
    pub fn current() -> Self {
        Self {
            session: SESSION_SCHEMA_VERSION,
            hero_draft: HERO_DRAFT_SCHEMA_VERSION,
            hero_character: HERO_CHARACTER_SCHEMA_VERSION,
            encounter: ENCOUNTER_SCHEMA_VERSION,
            rules_matrix: RULES_MATRIX_SCHEMA_VERSION,
            typed_ai_proposal: TYPED_AI_PROPOSAL_SCHEMA_VERSION,
            content_pack: CONTENT_PACK_SCHEMA_ID.to_owned(),
            typed_gm_request: TYPED_GM_REQUEST_SCHEMA_ID.to_owned(),
        }
    }

    fn is_current(&self) -> bool {
        self == &Self::current()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignContentPins {
    pub schema_version: u16,
    pub hero: HeroPins,
    pub prompt: CampaignPromptPin,
    pub schemas: CampaignSchemaPins,
    /// Fingerprint of the exact selected pack dependency closure activated by
    /// the server when the campaign was sealed. Unrelated installed packs do
    /// not participate in this durable identity.
    pub active_catalog_fingerprint: Sha256Digest,
}

impl CampaignContentPins {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CAMPAIGN_PINS_SCHEMA_VERSION {
            return invalid("campaign pin schema version is unsupported");
        }
        self.hero
            .validate()
            .map_err(|_| GameCoreError::InvalidCampaignPins {
                reason: "hero rules/content/theme pins are invalid",
            })?;
        if self.prompt.template_id != CAMPAIGN_PROMPT_TEMPLATE_ID
            || self.prompt.policy_id != CAMPAIGN_PROMPT_POLICY_ID
            || !is_valid_opaque_id(&self.prompt.template_id)
            || !is_valid_opaque_id(&self.prompt.policy_id)
        {
            return invalid("prompt template or policy pin is unsupported");
        }
        if !self.schemas.is_current() {
            return invalid("one or more schema pins are unsupported");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignPinSealReason {
    SelectedTheme,
    LegacySelectedTheme,
    LegacyDigestAlias,
    LegacyDefaultRainbound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedCampaignPins {
    pub seal_reason: CampaignPinSealReason,
    pub pins: CampaignContentPins,
    /// Original pre-release hero pins when an explicitly allowlisted digest
    /// alias was used to migrate a local development save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_source: Option<HeroPins>,
}

impl SealedCampaignPins {
    pub fn validate(&self) -> Result<()> {
        self.pins.validate()?;
        match (self.seal_reason, &self.legacy_source) {
            (CampaignPinSealReason::LegacyDigestAlias, Some(source))
                if source.is_legacy_dev_alias()
                    && source.theme_id == self.pins.hero.theme_id
                    && self.pins.hero == HeroPins::mvp(source.theme_id) =>
            {
                Ok(())
            }
            (CampaignPinSealReason::LegacyDigestAlias, _) => {
                invalid("legacy digest migration evidence is absent or unsupported")
            }
            (_, None) => Ok(()),
            (_, Some(_)) => invalid("legacy digest evidence requires the matching seal reason"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CampaignPinStatusDto {
    #[default]
    UnsealedCreatorScaffold,
    Sealed {
        evidence: Box<SealedCampaignPins>,
    },
}

impl CampaignPinStatusDto {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::UnsealedCreatorScaffold => Ok(()),
            Self::Sealed { evidence } => evidence.validate(),
        }
    }

    pub fn sealed(&self) -> Option<&SealedCampaignPins> {
        match self {
            Self::UnsealedCreatorScaffold => None,
            Self::Sealed { evidence } => Some(evidence.as_ref()),
        }
    }
}

fn invalid(reason: &'static str) -> Result<()> {
    Err(GameCoreError::InvalidCampaignPins { reason })
}

#[cfg(test)]
mod tests {
    use crate::hero::{HeroPins, ThemeId};

    use super::*;

    fn pins() -> CampaignContentPins {
        CampaignContentPins {
            schema_version: CAMPAIGN_PINS_SCHEMA_VERSION,
            hero: HeroPins::mvp(ThemeId::RainboundBorough),
            prompt: CampaignPromptPin {
                template_id: CAMPAIGN_PROMPT_TEMPLATE_ID.to_owned(),
                template_digest: Sha256Digest::from_bytes([1; 32]),
                policy_id: CAMPAIGN_PROMPT_POLICY_ID.to_owned(),
            },
            schemas: CampaignSchemaPins::current(),
            active_catalog_fingerprint: Sha256Digest::from_bytes([2; 32]),
        }
    }

    #[test]
    fn exact_campaign_pins_round_trip() {
        let evidence = SealedCampaignPins {
            seal_reason: CampaignPinSealReason::SelectedTheme,
            pins: pins(),
            legacy_source: None,
        };
        evidence.validate().unwrap();
        let json = serde_json::to_string(&evidence).unwrap();
        assert_eq!(
            serde_json::from_str::<SealedCampaignPins>(&json).unwrap(),
            evidence
        );
    }

    #[test]
    fn schema_or_prompt_drift_is_rejected() {
        let mut drifted = pins();
        drifted.schemas.encounter += 1;
        assert!(drifted.validate().is_err());

        let mut drifted = pins();
        drifted.prompt.policy_id = "policy:future".to_owned();
        assert!(drifted.validate().is_err());
    }
}
