use std::collections::BTreeMap;

use manchester_dnd_core::{
    CAMPAIGN_PINS_SCHEMA_VERSION, CAMPAIGN_PROMPT_POLICY_ID, CAMPAIGN_PROMPT_TEMPLATE_ID,
    CampaignContentPins, CampaignPromptPin, CampaignSchemaPins, Sha256Digest,
    hero::{
        CORE_CONTENT_PACK_ID, EMBERLINE_THEME_PACK_ID, HeroPins,
        LEGACY_DEV_CORE_CONTENT_PACK_DIGEST, LEGACY_DEV_EMBERLINE_THEME_PACK_DIGEST,
        LEGACY_DEV_RAINBOUND_THEME_PACK_DIGEST, LEGACY_PRE_LIVE_RULES_CORE_CONTENT_PACK_DIGEST,
        LEGACY_PRE_LIVE_RULES_EMBERLINE_THEME_PACK_DIGEST,
        LEGACY_PRE_LIVE_RULES_RAINBOUND_THEME_PACK_DIGEST, PackPin, RAINBOUND_THEME_PACK_ID,
        ThemeId,
    },
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{content::ActiveContentCatalog, typed_gm::TYPED_GM_PROMPT};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogPackPin {
    version: String,
    digest: Sha256Digest,
}

/// Process-wide immutable validator for campaign provenance. It contains no
/// filesystem handles and is safe to clone into the application service.
#[derive(Debug, Clone)]
pub struct CampaignPinRuntime {
    ruleset_id: String,
    packs: BTreeMap<String, CatalogPackPin>,
    prompt_digest: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CampaignPinValidationError {
    #[error("campaign pin payload is invalid")]
    InvalidPayload,
    #[error("a required pinned content pack is absent from the active catalog")]
    RequiredPackMissing,
    #[error("a pinned content pack version does not match the active catalog")]
    PackVersionMismatch,
    #[error("a pinned content pack digest does not match the active catalog")]
    PackDigestMismatch,
    #[error("the active catalog fingerprint differs from the sealed campaign")]
    CatalogFingerprintMismatch,
    #[error("the rules profile differs from the sealed campaign")]
    RulesetMismatch,
    #[error("the prompt template differs from the sealed campaign")]
    PromptMismatch,
}

impl CampaignPinRuntime {
    pub fn from_catalog(catalog: &ActiveContentCatalog) -> Self {
        let packs = catalog
            .packs()
            .iter()
            .map(|(id, pack)| {
                let identity = pack.identity();
                (
                    id.clone(),
                    CatalogPackPin {
                        version: identity.version.as_str().to_owned(),
                        digest: Sha256Digest::new(identity.digest.as_str())
                            .expect("validated content digests are canonical SHA-256 values"),
                    },
                )
            })
            .collect();
        Self {
            ruleset_id: catalog.ruleset_id().to_owned(),
            packs,
            prompt_digest: digest(TYPED_GM_PROMPT.as_bytes()),
        }
    }

    #[cfg(test)]
    pub(crate) fn bundled_for_tests() -> Self {
        use manchester_dnd_core::hero::{
            CORE_CONTENT_PACK_DIGEST, EMBERLINE_THEME_PACK_DIGEST, EMBERLINE_THEME_PACK_ID,
            MVP_PACK_VERSION, RAINBOUND_THEME_PACK_DIGEST, RAINBOUND_THEME_PACK_ID,
        };

        let packs = [
            (CORE_CONTENT_PACK_ID, CORE_CONTENT_PACK_DIGEST),
            (RAINBOUND_THEME_PACK_ID, RAINBOUND_THEME_PACK_DIGEST),
            (EMBERLINE_THEME_PACK_ID, EMBERLINE_THEME_PACK_DIGEST),
        ]
        .into_iter()
        .map(|(id, pack_digest)| {
            (
                id.to_owned(),
                CatalogPackPin {
                    version: MVP_PACK_VERSION.to_owned(),
                    digest: Sha256Digest::new(pack_digest)
                        .expect("compiled test pack digests are canonical"),
                },
            )
        })
        .collect();
        Self {
            ruleset_id: manchester_dnd_core::RULESET.as_str().to_owned(),
            packs,
            prompt_digest: digest(TYPED_GM_PROMPT.as_bytes()),
        }
    }

    pub fn pins_for_theme(
        &self,
        theme_id: ThemeId,
    ) -> Result<CampaignContentPins, CampaignPinValidationError> {
        let core = self
            .packs
            .get(CORE_CONTENT_PACK_ID)
            .ok_or(CampaignPinValidationError::RequiredPackMissing)?;
        let theme = self
            .packs
            .get(theme_id.pack_id())
            .ok_or(CampaignPinValidationError::RequiredPackMissing)?;
        let pins = CampaignContentPins {
            schema_version: CAMPAIGN_PINS_SCHEMA_VERSION,
            hero: HeroPins {
                ruleset_id: manchester_dnd_core::RULESET,
                core_content: PackPin {
                    pack_id: CORE_CONTENT_PACK_ID.to_owned(),
                    version: core.version.clone(),
                    digest: core.digest.clone(),
                },
                theme_id,
                theme: PackPin {
                    pack_id: theme_id.pack_id().to_owned(),
                    version: theme.version.clone(),
                    digest: theme.digest.clone(),
                },
            },
            prompt: CampaignPromptPin {
                template_id: CAMPAIGN_PROMPT_TEMPLATE_ID.to_owned(),
                template_digest: self.prompt_digest.clone(),
                policy_id: CAMPAIGN_PROMPT_POLICY_ID.to_owned(),
            },
            schemas: CampaignSchemaPins::current(),
            active_catalog_fingerprint: self.closure_fingerprint(theme_id)?,
        };
        self.validate(&pins)?;
        Ok(pins)
    }

    pub fn validate(&self, pins: &CampaignContentPins) -> Result<(), CampaignPinValidationError> {
        pins.validate()
            .map_err(|_| CampaignPinValidationError::InvalidPayload)?;
        if pins.hero.ruleset_id.as_str() != self.ruleset_id {
            return Err(CampaignPinValidationError::RulesetMismatch);
        }
        self.validate_pack(&pins.hero.core_content)?;
        self.validate_pack(&pins.hero.theme)?;
        if pins.prompt.template_digest != self.prompt_digest
            || pins.prompt.template_id != CAMPAIGN_PROMPT_TEMPLATE_ID
            || pins.prompt.policy_id != CAMPAIGN_PROMPT_POLICY_ID
        {
            return Err(CampaignPinValidationError::PromptMismatch);
        }
        let expected_fingerprint = if pins.hero.is_legacy_dev_alias() {
            closure_fingerprint_for_hero(&pins.hero)
        } else {
            self.closure_fingerprint(pins.hero.theme_id)?
        };
        if pins.active_catalog_fingerprint != expected_fingerprint {
            return Err(CampaignPinValidationError::CatalogFingerprintMismatch);
        }
        Ok(())
    }

    fn validate_pack(&self, pin: &PackPin) -> Result<(), CampaignPinValidationError> {
        let active = self
            .packs
            .get(&pin.pack_id)
            .ok_or(CampaignPinValidationError::RequiredPackMissing)?;
        if pin.version != active.version {
            return Err(CampaignPinValidationError::PackVersionMismatch);
        }
        if pin.digest != active.digest && !is_retained_dev_alias(pin) {
            return Err(CampaignPinValidationError::PackDigestMismatch);
        }
        Ok(())
    }

    fn closure_fingerprint(
        &self,
        theme_id: ThemeId,
    ) -> Result<Sha256Digest, CampaignPinValidationError> {
        let core = self
            .packs
            .get(CORE_CONTENT_PACK_ID)
            .ok_or(CampaignPinValidationError::RequiredPackMissing)?;
        let theme = self
            .packs
            .get(theme_id.pack_id())
            .ok_or(CampaignPinValidationError::RequiredPackMissing)?;
        let canonical = format!(
            "selected-content-closure/v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
            self.ruleset_id,
            CORE_CONTENT_PACK_ID,
            core.version,
            core.digest,
            theme_id.pack_id(),
            theme.version,
            theme.digest,
        );
        Ok(digest(canonical.as_bytes()))
    }
}

fn is_retained_dev_alias(pin: &PackPin) -> bool {
    matches!(
        (pin.pack_id.as_str(), pin.digest.as_str()),
        (CORE_CONTENT_PACK_ID, LEGACY_DEV_CORE_CONTENT_PACK_DIGEST)
            | (
                CORE_CONTENT_PACK_ID,
                LEGACY_PRE_LIVE_RULES_CORE_CONTENT_PACK_DIGEST
            )
            | (
                RAINBOUND_THEME_PACK_ID,
                LEGACY_DEV_RAINBOUND_THEME_PACK_DIGEST
            )
            | (
                RAINBOUND_THEME_PACK_ID,
                LEGACY_PRE_LIVE_RULES_RAINBOUND_THEME_PACK_DIGEST
            )
            | (
                EMBERLINE_THEME_PACK_ID,
                LEGACY_DEV_EMBERLINE_THEME_PACK_DIGEST
            )
            | (
                EMBERLINE_THEME_PACK_ID,
                LEGACY_PRE_LIVE_RULES_EMBERLINE_THEME_PACK_DIGEST
            )
    )
}

fn closure_fingerprint_for_hero(hero: &HeroPins) -> Sha256Digest {
    let canonical = format!(
        "selected-content-closure/v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        hero.ruleset_id,
        hero.core_content.pack_id,
        hero.core_content.version,
        hero.core_content.digest,
        hero.theme.pack_id,
        hero.theme.version,
        hero.theme.digest,
    );
    digest(canonical.as_bytes())
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use manchester_dnd_core::hero::{CORE_CONTENT_PACK_DIGEST, EMBERLINE_THEME_PACK_DIGEST};

    use super::*;

    fn runtime() -> CampaignPinRuntime {
        CampaignPinRuntime::bundled_for_tests()
    }

    #[test]
    fn both_campaign_themes_receive_exact_active_catalog_pins() {
        let runtime = runtime();
        for theme in ThemeId::ALL {
            let pins = runtime.pins_for_theme(theme).unwrap();
            runtime.validate(&pins).unwrap();
            assert_eq!(pins.hero.theme.pack_id, theme.pack_id());
        }
    }

    #[test]
    fn selected_pack_removal_and_closure_drift_fail_closed() {
        let runtime = runtime();
        let pins = runtime.pins_for_theme(ThemeId::EmberlineArchive).unwrap();

        let mut removed = runtime.clone();
        removed.packs.remove(pins.hero.theme_id.pack_id());
        assert_eq!(
            removed.validate(&pins),
            Err(CampaignPinValidationError::RequiredPackMissing)
        );

        let mut drifted_catalog = runtime.clone();
        drifted_catalog
            .packs
            .get_mut(CORE_CONTENT_PACK_ID)
            .unwrap()
            .digest = Sha256Digest::from_bytes([9; 32]);
        assert_eq!(
            drifted_catalog.validate(&pins),
            Err(CampaignPinValidationError::PackDigestMismatch)
        );
    }

    #[test]
    fn unused_theme_removal_or_unrelated_pack_addition_does_not_change_resume_closure() {
        let runtime = runtime();
        let pins = runtime.pins_for_theme(ThemeId::RainboundBorough).unwrap();

        let mut without_unused_theme = runtime.clone();
        without_unused_theme
            .packs
            .remove(ThemeId::EmberlineArchive.pack_id());
        without_unused_theme.validate(&pins).unwrap();

        let mut with_extra_pack = runtime;
        with_extra_pack.packs.insert(
            "dev.example.unrelated-pack".to_owned(),
            CatalogPackPin {
                version: "9.9.9".to_owned(),
                digest: Sha256Digest::from_bytes([8; 32]),
            },
        );
        with_extra_pack.validate(&pins).unwrap();
    }

    #[test]
    fn version_and_digest_drift_are_rejected() {
        let runtime = runtime();
        let pins = runtime.pins_for_theme(ThemeId::RainboundBorough).unwrap();

        let mut version_drift = runtime.clone();
        version_drift
            .packs
            .get_mut(CORE_CONTENT_PACK_ID)
            .unwrap()
            .version = "1.0.1".to_owned();
        assert_eq!(
            version_drift.validate(&pins),
            Err(CampaignPinValidationError::PackVersionMismatch)
        );

        let mut digest_drift = runtime.clone();
        digest_drift
            .packs
            .get_mut(CORE_CONTENT_PACK_ID)
            .unwrap()
            .digest = Sha256Digest::new(EMBERLINE_THEME_PACK_DIGEST).unwrap();
        assert_ne!(CORE_CONTENT_PACK_DIGEST, EMBERLINE_THEME_PACK_DIGEST);
        assert_eq!(
            digest_drift.validate(&pins),
            Err(CampaignPinValidationError::PackDigestMismatch)
        );
    }
}
