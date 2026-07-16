use manchester_dnd_core::{
    CAMPAIGN_PINS_SCHEMA_VERSION, CampaignContentPins, CampaignPinSealReason, SealedCampaignPins,
    hero::HeroPins, is_valid_opaque_id,
};
use sqlx::{Postgres, Row, Transaction};

use super::{PostgresRepository, serialize};
use crate::error::RepositoryError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCampaignPins {
    pub campaign_session_id: String,
    pub evidence: SealedCampaignPins,
    pub created_at: String,
}

impl PostgresRepository {
    pub async fn load_campaign_pins(
        &self,
        campaign_session_id: &str,
    ) -> Result<Option<StoredCampaignPins>, RepositoryError> {
        validate_campaign_id(campaign_session_id)?;
        let row = sqlx::query(
            "SELECT campaign_session_id, schema_version, seal_reason,
                    payload_json::text AS payload_json,
                    legacy_source_json::text AS legacy_source_json,
                    created_at::text AS created_at
             FROM campaign_content_pins
             WHERE campaign_session_id = $1",
        )
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(stored_pins_from_row).transpose()
    }

    pub(crate) async fn campaign_pin_legacy_eligible(
        &self,
        campaign_session_id: &str,
    ) -> Result<bool, RepositoryError> {
        validate_campaign_id(campaign_session_id)?;
        sqlx::query_scalar(
            "SELECT content_pin_legacy_eligible
             FROM campaign_sessions WHERE id = $1",
        )
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign session",
            id: campaign_session_id.to_owned(),
        })
    }

    pub(crate) async fn seal_legacy_campaign_pins(
        &self,
        campaign_session_id: &str,
        evidence: &SealedCampaignPins,
    ) -> Result<StoredCampaignPins, RepositoryError> {
        if matches!(evidence.seal_reason, CampaignPinSealReason::SelectedTheme) {
            return invalid(
                campaign_session_id,
                "legacy migration must record an explicit legacy seal reason",
            );
        }
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let stored = seal_campaign_pins_in_transaction(
            &mut transaction,
            campaign_session_id,
            evidence,
            SealAuthority::LegacyMigration,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(stored)
    }

    #[cfg(test)]
    pub(crate) async fn seal_campaign_pins_for_test(
        &self,
        campaign_session_id: &str,
        evidence: &SealedCampaignPins,
    ) -> Result<StoredCampaignPins, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let stored = seal_campaign_pins_in_transaction(
            &mut transaction,
            campaign_session_id,
            evidence,
            SealAuthority::ThemeSelection,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(stored)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SealAuthority {
    ThemeSelection,
    LegacyMigration,
}

pub(super) async fn seal_campaign_pins_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    campaign_session_id: &str,
    evidence: &SealedCampaignPins,
    authority: SealAuthority,
) -> Result<StoredCampaignPins, RepositoryError> {
    validate_campaign_id(campaign_session_id)?;
    evidence
        .validate()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            id: campaign_session_id.to_owned(),
            reason: "campaign pins failed domain validation",
        })?;
    match authority {
        SealAuthority::ThemeSelection
            if !matches!(evidence.seal_reason, CampaignPinSealReason::SelectedTheme) =>
        {
            return invalid(
                campaign_session_id,
                "theme selection must use the selected-theme seal reason",
            );
        }
        SealAuthority::LegacyMigration
            if matches!(evidence.seal_reason, CampaignPinSealReason::SelectedTheme) =>
        {
            return invalid(
                campaign_session_id,
                "legacy migration must use a legacy seal reason",
            );
        }
        SealAuthority::ThemeSelection | SealAuthority::LegacyMigration => {}
    }

    let legacy_eligible: bool = sqlx::query_scalar(
        "SELECT content_pin_legacy_eligible
         FROM campaign_sessions WHERE id = $1 FOR UPDATE",
    )
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?
    .ok_or_else(|| RepositoryError::NotFound {
        entity: "campaign session",
        id: campaign_session_id.to_owned(),
    })?;
    if authority == SealAuthority::LegacyMigration && !legacy_eligible {
        return invalid(
            campaign_session_id,
            "new campaign scaffolds cannot use the legacy default migration",
        );
    }

    if let Some(row) = sqlx::query(
        "SELECT campaign_session_id, schema_version, seal_reason,
                payload_json::text AS payload_json,
                legacy_source_json::text AS legacy_source_json,
                created_at::text AS created_at
         FROM campaign_content_pins
         WHERE campaign_session_id = $1",
    )
    .bind(campaign_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?
    {
        let stored = stored_pins_from_row(row)?;
        if stored.evidence.pins != evidence.pins {
            return invalid(
                campaign_session_id,
                "sealed campaign pins are immutable and cannot change theme or content",
            );
        }
        return Ok(stored);
    }

    let payload = serialize("campaign content pins", &evidence.pins)?;
    let legacy_source_payload = evidence
        .legacy_source
        .as_ref()
        .map(|source| serialize("legacy campaign content pins", source))
        .transpose()?;
    let row = sqlx::query(
        "INSERT INTO campaign_content_pins
         (campaign_session_id, schema_version, seal_reason, payload_json, legacy_source_json)
         VALUES ($1, $2, $3, $4::jsonb, $5::jsonb)
         RETURNING created_at::text AS created_at",
    )
    .bind(campaign_session_id)
    .bind(i64::from(CAMPAIGN_PINS_SCHEMA_VERSION))
    .bind(seal_reason_as_str(evidence.seal_reason))
    .bind(payload)
    .bind(legacy_source_payload)
    .fetch_one(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?;
    sqlx::query(
        "UPDATE campaign_sessions
         SET content_pin_legacy_eligible = FALSE
         WHERE id = $1",
    )
    .bind(campaign_session_id)
    .execute(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?;

    Ok(StoredCampaignPins {
        campaign_session_id: campaign_session_id.to_owned(),
        evidence: evidence.clone(),
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn stored_pins_from_row(row: sqlx::postgres::PgRow) -> Result<StoredCampaignPins, RepositoryError> {
    let campaign_session_id: String = row
        .try_get("campaign_session_id")
        .map_err(RepositoryError::Database)?;
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(RepositoryError::Database)?;
    if schema_version != i64::from(CAMPAIGN_PINS_SCHEMA_VERSION) {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "campaign content pins",
            found: u32::try_from(schema_version).unwrap_or(u32::MAX),
            supported: u32::from(CAMPAIGN_PINS_SCHEMA_VERSION),
        });
    }
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let pins: CampaignContentPins = serde_json::from_str(&payload_json).map_err(|source| {
        RepositoryError::InvalidStoredData {
            entity: "campaign content pins",
            id: campaign_session_id.clone(),
            source,
        }
    })?;
    let seal_reason = parse_seal_reason(
        row.try_get::<String, _>("seal_reason")
            .map_err(RepositoryError::Database)?
            .as_str(),
        &campaign_session_id,
    )?;
    let legacy_source_json: Option<String> = row
        .try_get("legacy_source_json")
        .map_err(RepositoryError::Database)?;
    let legacy_source: Option<HeroPins> = legacy_source_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|source| RepositoryError::InvalidStoredData {
                entity: "legacy campaign content pins",
                id: campaign_session_id.clone(),
                source,
            })
        })
        .transpose()?;
    let evidence = SealedCampaignPins {
        seal_reason,
        pins,
        legacy_source,
    };
    evidence
        .validate()
        .map_err(|_| RepositoryError::InvalidDomainState {
            entity: "campaign content pins",
            id: campaign_session_id.clone(),
            reason: "stored campaign pins failed domain validation",
        })?;
    Ok(StoredCampaignPins {
        campaign_session_id,
        evidence,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

const fn seal_reason_as_str(reason: CampaignPinSealReason) -> &'static str {
    match reason {
        CampaignPinSealReason::SelectedTheme => "selected_theme",
        CampaignPinSealReason::LegacySelectedTheme => "legacy_selected_theme",
        CampaignPinSealReason::LegacyDigestAlias => "legacy_digest_alias",
        CampaignPinSealReason::LegacyDefaultRainbound => "legacy_default_rainbound",
    }
}

fn parse_seal_reason(
    value: &str,
    campaign_session_id: &str,
) -> Result<CampaignPinSealReason, RepositoryError> {
    match value {
        "selected_theme" => Ok(CampaignPinSealReason::SelectedTheme),
        "legacy_selected_theme" => Ok(CampaignPinSealReason::LegacySelectedTheme),
        "legacy_digest_alias" => Ok(CampaignPinSealReason::LegacyDigestAlias),
        "legacy_default_rainbound" => Ok(CampaignPinSealReason::LegacyDefaultRainbound),
        _ => invalid(campaign_session_id, "stored seal reason is unsupported"),
    }
}

fn validate_campaign_id(campaign_session_id: &str) -> Result<(), RepositoryError> {
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
