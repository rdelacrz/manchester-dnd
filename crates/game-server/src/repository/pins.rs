use manchester_dnd_core::{CampaignPinSealReason, SealedCampaignPins, is_valid_opaque_id};
use mongodb::{
    ClientSession, Collection,
    bson::{Bson, DateTime, doc},
};

use super::{
    CampaignDocument, MongoRepository, active_campaign_filter, date_string, mongo_error,
    validate_account_id,
};
use crate::error::{PersistenceError, RepositoryError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCampaignPins {
    pub campaign_session_id: String,
    pub evidence: SealedCampaignPins,
    pub created_at: String,
}

impl MongoRepository {
    pub async fn load_campaign_pins(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
    ) -> Result<Option<StoredCampaignPins>, RepositoryError> {
        validate_pin_scope(actor_account_id, campaign_session_id)?;
        let stored = self
            .campaigns()
            .find_one(active_campaign_filter(
                actor_account_id,
                campaign_session_id,
            ))
            .await
            .map_err(|error| mongo_error("load campaign pins", error))?;
        stored
            .and_then(|campaign| {
                campaign
                    .rules_snapshot
                    .get("campaign_pins")
                    .cloned()
                    .map(|pins| (campaign, pins))
            })
            .map(|(campaign, pins)| stored_pins_from_bson(&campaign, pins))
            .transpose()
    }

    #[cfg(test)]
    pub(crate) async fn seal_campaign_pins_for_test(
        &self,
        actor_account_id: &str,
        campaign_session_id: &str,
        evidence: &SealedCampaignPins,
    ) -> Result<StoredCampaignPins, RepositoryError> {
        validate_seal(actor_account_id, campaign_session_id, evidence)?;
        if let Some(stored) = self
            .load_campaign_pins(actor_account_id, campaign_session_id)
            .await?
        {
            if stored.evidence.pins != evidence.pins {
                return invalid(campaign_session_id, "sealed campaign pins are immutable");
            }
            return Ok(stored);
        }
        let campaigns = self.campaigns();
        let actor = actor_account_id.to_owned();
        let campaign_id = campaign_session_id.to_owned();
        let evidence_for_write = evidence.clone();
        let sealed_at = DateTime::now();
        let result = self
            .store()
            .with_transaction(move |client_session| {
                let campaigns = campaigns.clone();
                let actor = actor.clone();
                let campaign_id = campaign_id.clone();
                let evidence = evidence_for_write.clone();
                Box::pin(async move {
                    seal_campaign_pins_in_transaction(
                        &campaigns,
                        client_session,
                        &actor,
                        &campaign_id,
                        &evidence,
                        sealed_at,
                    )
                    .await
                })
            })
            .await
            .map_err(super::map_persistence)?;
        Ok(StoredCampaignPins {
            campaign_session_id: campaign_session_id.to_owned(),
            evidence: evidence.clone(),
            created_at: date_string(result),
        })
    }
}

pub(super) async fn seal_campaign_pins_in_transaction(
    campaigns: &Collection<CampaignDocument>,
    client_session: &mut ClientSession,
    actor_account_id: &str,
    campaign_session_id: &str,
    evidence: &SealedCampaignPins,
    sealed_at: DateTime,
) -> Result<DateTime, PersistenceError> {
    let campaign = campaigns
        .find_one(active_campaign_filter(
            actor_account_id,
            campaign_session_id,
        ))
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("authorize campaign pin seal", error))?
        .ok_or_else(|| PersistenceError::NotFound {
            entity: "campaign",
            id: campaign_session_id.to_owned(),
        })?;
    if let Some(stored) = campaign.rules_snapshot.get("campaign_pins").cloned() {
        let stored: SealedCampaignPins =
            mongodb::bson::from_bson(stored).map_err(|error| PersistenceError::SchemaDrift {
                collection: "campaigns".to_owned(),
                detail: format!("stored campaign pins are invalid: {error}"),
            })?;
        if stored == *evidence {
            return campaign
                .rules_snapshot
                .get_datetime("sealed_at")
                .copied()
                .map_err(|_| PersistenceError::SchemaDrift {
                    collection: "campaigns".to_owned(),
                    detail: "sealed campaign pins are missing their BSON date".to_owned(),
                });
        }
        return Err(PersistenceError::RevisionConflict {
            entity: "campaign content pins",
            id: campaign_session_id.to_owned(),
            expected: u64::try_from(campaign.revision).unwrap_or_default(),
            actual: u64::try_from(campaign.revision).unwrap_or_default(),
        });
    }
    let pins = mongodb::bson::to_bson(evidence).map_err(PersistenceError::BsonEncoding)?;
    let mut filter = active_campaign_filter(actor_account_id, campaign_session_id);
    filter.insert("rules_snapshot.campaign_pins", doc! { "$exists": false });
    let result = campaigns
        .update_one(
            filter,
            doc! {
                "$set": {
                    "rules_snapshot.state": "sealed",
                    "rules_snapshot.campaign_pins": pins,
                    "rules_snapshot.sealed_at": sealed_at,
                    "updated_at": sealed_at,
                },
                "$inc": { "revision": 1_i64 },
            },
        )
        .session(&mut *client_session)
        .await
        .map_err(|error| PersistenceError::mongo("seal campaign pins", error))?;
    if result.modified_count != 1 {
        return Err(PersistenceError::RevisionConflict {
            entity: "campaign content pins",
            id: campaign_session_id.to_owned(),
            expected: 0,
            actual: 1,
        });
    }
    Ok(sealed_at)
}

pub(super) fn validate_seal(
    actor_account_id: &str,
    campaign_session_id: &str,
    evidence: &SealedCampaignPins,
) -> Result<(), RepositoryError> {
    validate_pin_scope(actor_account_id, campaign_session_id)?;
    evidence
        .validate()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            id: campaign_session_id.to_owned(),
            reason: "campaign pins failed domain validation",
        })?;
    if !matches!(evidence.seal_reason, CampaignPinSealReason::SelectedTheme)
        || evidence.legacy_source.is_some()
    {
        return invalid(
            campaign_session_id,
            "greenfield campaigns accept selected-theme evidence only",
        );
    }
    Ok(())
}

fn stored_pins_from_bson(
    campaign: &CampaignDocument,
    pins: Bson,
) -> Result<StoredCampaignPins, RepositoryError> {
    let evidence: SealedCampaignPins =
        mongodb::bson::from_bson(pins).map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            id: campaign.id.clone(),
            reason: "stored campaign pins have an invalid BSON shape",
        })?;
    evidence
        .validate()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            id: campaign.id.clone(),
            reason: "stored campaign pins failed domain validation",
        })?;
    if !matches!(evidence.seal_reason, CampaignPinSealReason::SelectedTheme)
        || evidence.legacy_source.is_some()
    {
        return invalid(
            &campaign.id,
            "stored campaign pins contain retired compatibility evidence",
        );
    }
    let sealed_at = campaign
        .rules_snapshot
        .get_datetime("sealed_at")
        .copied()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            id: campaign.id.clone(),
            reason: "stored campaign pins are missing their BSON seal date",
        })?;
    Ok(StoredCampaignPins {
        campaign_session_id: campaign.id.clone(),
        evidence,
        created_at: date_string(sealed_at),
    })
}

fn validate_pin_scope(
    actor_account_id: &str,
    campaign_session_id: &str,
) -> Result<(), RepositoryError> {
    validate_account_id(actor_account_id)?;
    if !is_valid_opaque_id(campaign_session_id) {
        return invalid(
            campaign_session_id,
            "campaign id must be a valid opaque identifier",
        );
    }
    Ok(())
}

fn invalid<T>(campaign_session_id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity: "campaign content pins",
        id: campaign_session_id.to_owned(),
        reason,
    })
}
