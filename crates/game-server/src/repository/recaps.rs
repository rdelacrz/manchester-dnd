//! Durable owner-private recaps derived only from committed turn audits.

use manchester_dnd_core::{
    SESSION_SCHEMA_VERSION, SessionEventDto, SessionEventPayload, Sha256Digest, is_valid_opaque_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use super::{PostgresRepository, from_i64, to_i64};
use crate::error::RepositoryError;

pub const PRIVATE_RECAP_SCHEMA_VERSION: u16 = 1;
const PRIVATE_RECAP_TEMPLATE_ID: &str = "private-recap-v1";
const MAX_PRIVATE_RECAP_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratePrivateRecapCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_campaign_revision: u64,
    pub idempotency_key: String,
}

impl GeneratePrivateRecapCommand {
    pub fn validate(&self) -> Result<(), RepositoryError> {
        if self.schema_version != PRIVATE_RECAP_SCHEMA_VERSION {
            return Err(RepositoryError::UnsupportedSchemaVersion {
                entity: "private recap command",
                found: u32::from(self.schema_version),
                supported: u32::from(PRIVATE_RECAP_SCHEMA_VERSION),
            });
        }
        if !is_valid_opaque_id(&self.campaign_session_id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.expected_campaign_revision == 0
        {
            return invalid(
                "private recap command",
                &self.campaign_session_id,
                "identity or expected revision is invalid",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPrivateRecap {
    pub schema_version: u16,
    pub id: String,
    pub campaign_session_id: String,
    pub campaign_revision: u64,
    pub idempotency_key: String,
    pub request_fingerprint: Sha256Digest,
    pub first_turn_number: Option<u64>,
    pub last_turn_number: Option<u64>,
    pub source_audit_count: u64,
    pub source_audit_digest: Sha256Digest,
    pub template_id: String,
    pub body: String,
    pub body_digest: Sha256Digest,
    pub created_at: String,
}

impl CampaignPrivateRecap {
    pub(crate) fn validate_for_campaign(
        &self,
        campaign_session_id: &str,
        maximum_campaign_revision: u64,
    ) -> Result<(), RepositoryError> {
        let valid_turn_range = match self.source_audit_count {
            0 => self.first_turn_number.is_none() && self.last_turn_number.is_none(),
            _ => self
                .first_turn_number
                .zip(self.last_turn_number)
                .is_some_and(|(first, last)| first > 0 && last >= first),
        };
        if self.schema_version != PRIVATE_RECAP_SCHEMA_VERSION
            || self.campaign_session_id != campaign_session_id
            || !is_valid_opaque_id(&self.id)
            || !is_valid_opaque_id(&self.idempotency_key)
            || self.campaign_revision == 0
            || self.campaign_revision > maximum_campaign_revision
            || !valid_turn_range
            || self.template_id != PRIVATE_RECAP_TEMPLATE_ID
            || self.body.is_empty()
            || self.body.len() > MAX_PRIVATE_RECAP_BYTES
            || digest(self.body.as_bytes()) != self.body_digest
        {
            return invalid(
                "private campaign recap",
                &self.id,
                "identity, source range, provenance, or body integrity is invalid",
            );
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct RecapAuditDigestInput<'a> {
    id: &'a str,
    turn_number: u64,
    event: &'a SessionEventDto,
}

struct RecapSourceTurn {
    id: String,
    turn_number: u64,
    event: SessionEventDto,
}

impl PostgresRepository {
    pub async fn generate_private_recap(
        &self,
        owner_key: &str,
        command: &GeneratePrivateRecapCommand,
    ) -> Result<CampaignPrivateRecap, RepositoryError> {
        validate_owner(owner_key)?;
        command.validate()?;
        let request_fingerprint = command_fingerprint(command)?;
        let mut transaction = self.pool.begin().await.map_err(RepositoryError::Database)?;

        if let Some(existing) = recap_by_idempotency_key(
            &mut transaction,
            owner_key,
            &command.campaign_session_id,
            &command.idempotency_key,
        )
        .await?
        {
            if existing.request_fingerprint != request_fingerprint {
                return invalid(
                    "private recap command",
                    &command.idempotency_key,
                    "idempotency key was reused with different input",
                );
            }
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(existing);
        }

        let campaign = sqlx::query(
            "SELECT revision, payload_json->>'title' AS title
             FROM campaign_sessions
             WHERE owner_key = $1 AND id = $2
             FOR UPDATE",
        )
        .bind(owner_key)
        .bind(&command.campaign_session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign session",
            id: command.campaign_session_id.clone(),
        })?;
        let current_revision = from_i64(
            campaign
                .try_get("revision")
                .map_err(RepositoryError::Database)?,
            "campaign revision",
        )?;
        if current_revision != command.expected_campaign_revision {
            return Err(RepositoryError::RevisionConflict {
                entity: "campaign session",
                id: command.campaign_session_id.clone(),
                expected: command.expected_campaign_revision,
                actual: current_revision,
            });
        }

        if let Some(existing) = recap_by_campaign_revision(
            &mut transaction,
            owner_key,
            &command.campaign_session_id,
            current_revision,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(RepositoryError::Database)?;
            return Ok(existing);
        }

        let title: String = campaign
            .try_get("title")
            .map_err(RepositoryError::Database)?;
        let rows = sqlx::query(
            "SELECT id, turn_number, payload_json::text AS payload_json
             FROM turn_audits
             WHERE campaign_session_id = $1
             ORDER BY turn_number, id",
        )
        .bind(&command.campaign_session_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let turns = rows
            .into_iter()
            .map(source_turn_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let source_audit_digest = audit_digest(&turns)?;
        let body = render_recap(&title, current_revision, &source_audit_digest, &turns)?;
        let body_digest = digest(body.as_bytes());
        let id = format!(
            "private-recap:{}",
            &request_fingerprint.as_str()["sha256:".len().."sha256:".len() + 32]
        );
        let first_turn_number = turns.first().map(|turn| turn.turn_number);
        let last_turn_number = turns.last().map(|turn| turn.turn_number);

        let row = sqlx::query(
            "INSERT INTO campaign_private_recaps
             (id, campaign_session_id, owner_key, schema_version,
              campaign_revision, idempotency_key, request_fingerprint,
              first_turn_number, last_turn_number, source_audit_count,
              source_audit_digest, template_id, body, body_digest)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             RETURNING id, campaign_session_id, schema_version, campaign_revision,
                       idempotency_key, request_fingerprint, first_turn_number,
                       last_turn_number, source_audit_count, source_audit_digest,
                       template_id, body, body_digest, created_at::text AS created_at",
        )
        .bind(&id)
        .bind(&command.campaign_session_id)
        .bind(owner_key)
        .bind(i64::from(PRIVATE_RECAP_SCHEMA_VERSION))
        .bind(to_i64(current_revision, "private recap campaign revision")?)
        .bind(&command.idempotency_key)
        .bind(request_fingerprint.as_str())
        .bind(
            first_turn_number
                .map(|value| to_i64(value, "private recap first turn"))
                .transpose()?,
        )
        .bind(
            last_turn_number
                .map(|value| to_i64(value, "private recap last turn"))
                .transpose()?,
        )
        .bind(to_i64(
            u64::try_from(turns.len()).map_err(|_| RepositoryError::NumericRange {
                field: "private recap source audit count",
            })?,
            "private recap source audit count",
        )?)
        .bind(source_audit_digest.as_str())
        .bind(PRIVATE_RECAP_TEMPLATE_ID)
        .bind(&body)
        .bind(body_digest.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(RepositoryError::Database)?;
        let recap = private_recap_from_row(row)?;
        recap.validate_for_campaign(&command.campaign_session_id, current_revision)?;
        transaction
            .commit()
            .await
            .map_err(RepositoryError::Database)?;
        Ok(recap)
    }

    pub async fn load_latest_private_recap(
        &self,
        owner_key: &str,
        campaign_session_id: &str,
    ) -> Result<Option<CampaignPrivateRecap>, RepositoryError> {
        validate_owner(owner_key)?;
        if !is_valid_opaque_id(campaign_session_id) {
            return invalid(
                "campaign session",
                campaign_session_id,
                "campaign identity is invalid",
            );
        }
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM campaign_sessions WHERE owner_key = $1 AND id = $2",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        let revision = revision.ok_or_else(|| RepositoryError::NotFound {
            entity: "campaign session",
            id: campaign_session_id.to_owned(),
        })?;
        let maximum_revision = from_i64(revision, "campaign revision")?;
        let row = sqlx::query(
            "SELECT id, campaign_session_id, schema_version, campaign_revision,
                    idempotency_key, request_fingerprint, first_turn_number,
                    last_turn_number, source_audit_count, source_audit_digest,
                    template_id, body, body_digest, created_at::text AS created_at
             FROM campaign_private_recaps
             WHERE owner_key = $1 AND campaign_session_id = $2
             ORDER BY campaign_revision DESC, created_at DESC, id DESC
             LIMIT 1",
        )
        .bind(owner_key)
        .bind(campaign_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::Database)?;
        row.map(private_recap_from_row)
            .transpose()?
            .map(|recap| {
                recap.validate_for_campaign(campaign_session_id, maximum_revision)?;
                Ok(recap)
            })
            .transpose()
    }
}

async fn recap_by_idempotency_key(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: &str,
    campaign_session_id: &str,
    idempotency_key: &str,
) -> Result<Option<CampaignPrivateRecap>, RepositoryError> {
    let row = sqlx::query(
        "SELECT id, campaign_session_id, schema_version, campaign_revision,
                idempotency_key, request_fingerprint, first_turn_number,
                last_turn_number, source_audit_count, source_audit_digest,
                template_id, body, body_digest, created_at::text AS created_at
         FROM campaign_private_recaps
         WHERE owner_key = $1 AND campaign_session_id = $2 AND idempotency_key = $3",
    )
    .bind(owner_key)
    .bind(campaign_session_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?;
    row.map(private_recap_from_row).transpose()
}

async fn recap_by_campaign_revision(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: &str,
    campaign_session_id: &str,
    campaign_revision: u64,
) -> Result<Option<CampaignPrivateRecap>, RepositoryError> {
    let row = sqlx::query(
        "SELECT id, campaign_session_id, schema_version, campaign_revision,
                idempotency_key, request_fingerprint, first_turn_number,
                last_turn_number, source_audit_count, source_audit_digest,
                template_id, body, body_digest, created_at::text AS created_at
         FROM campaign_private_recaps
         WHERE owner_key = $1 AND campaign_session_id = $2 AND campaign_revision = $3",
    )
    .bind(owner_key)
    .bind(campaign_session_id)
    .bind(to_i64(
        campaign_revision,
        "private recap campaign revision",
    )?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(RepositoryError::Database)?;
    row.map(private_recap_from_row).transpose()
}

pub(crate) fn private_recap_from_row(row: PgRow) -> Result<CampaignPrivateRecap, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    Ok(CampaignPrivateRecap {
        schema_version: u16::try_from(from_i64(
            row.try_get("schema_version")
                .map_err(RepositoryError::Database)?,
            "private recap schema version",
        )?)
        .map_err(|_| RepositoryError::NumericRange {
            field: "private recap schema version",
        })?,
        id: id.clone(),
        campaign_session_id: row
            .try_get("campaign_session_id")
            .map_err(RepositoryError::Database)?,
        campaign_revision: from_i64(
            row.try_get("campaign_revision")
                .map_err(RepositoryError::Database)?,
            "private recap campaign revision",
        )?,
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(RepositoryError::Database)?,
        request_fingerprint: digest_from_row(&row, "request_fingerprint", &id)?,
        first_turn_number: row
            .try_get::<Option<i64>, _>("first_turn_number")
            .map_err(RepositoryError::Database)?
            .map(|value| from_i64(value, "private recap first turn"))
            .transpose()?,
        last_turn_number: row
            .try_get::<Option<i64>, _>("last_turn_number")
            .map_err(RepositoryError::Database)?
            .map(|value| from_i64(value, "private recap last turn"))
            .transpose()?,
        source_audit_count: from_i64(
            row.try_get("source_audit_count")
                .map_err(RepositoryError::Database)?,
            "private recap source audit count",
        )?,
        source_audit_digest: digest_from_row(&row, "source_audit_digest", &id)?,
        template_id: row
            .try_get("template_id")
            .map_err(RepositoryError::Database)?,
        body: row.try_get("body").map_err(RepositoryError::Database)?,
        body_digest: digest_from_row(&row, "body_digest", &id)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
    })
}

fn source_turn_from_row(row: PgRow) -> Result<RecapSourceTurn, RepositoryError> {
    let id: String = row.try_get("id").map_err(RepositoryError::Database)?;
    let payload: String = row
        .try_get("payload_json")
        .map_err(RepositoryError::Database)?;
    let event: SessionEventDto =
        serde_json::from_str(&payload).map_err(|source| RepositoryError::InvalidStoredData {
            entity: "turn audit",
            id: id.clone(),
            source,
        })?;
    if event.schema_version != SESSION_SCHEMA_VERSION {
        return Err(RepositoryError::UnsupportedSchemaVersion {
            entity: "turn audit",
            found: u32::from(event.schema_version),
            supported: u32::from(SESSION_SCHEMA_VERSION),
        });
    }
    Ok(RecapSourceTurn {
        id,
        turn_number: from_i64(
            row.try_get("turn_number")
                .map_err(RepositoryError::Database)?,
            "turn number",
        )?,
        event,
    })
}

fn audit_digest(turns: &[RecapSourceTurn]) -> Result<Sha256Digest, RepositoryError> {
    let input = turns
        .iter()
        .map(|turn| RecapAuditDigestInput {
            id: &turn.id,
            turn_number: turn.turn_number,
            event: &turn.event,
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_vec(&input).map_err(|source| RepositoryError::Serialize {
        entity: "private recap audit digest input",
        source,
    })?;
    Ok(digest(&encoded))
}

fn render_recap(
    title: &str,
    campaign_revision: u64,
    source_audit_digest: &Sha256Digest,
    turns: &[RecapSourceTurn],
) -> Result<String, RepositoryError> {
    let mut body = format!(
        "# Private campaign recap\n\nCampaign: {}\n\nCampaign revision: {campaign_revision}\n\nProvenance: {PRIVATE_RECAP_TEMPLATE_ID}; {} committed audit{}; source {}.\n\n",
        escape_markdown_line(title),
        turns.len(),
        if turns.len() == 1 { "" } else { "s" },
        source_audit_digest,
    );
    if turns.is_empty() {
        body.push_str("No committed turns have been recorded yet.\n");
    } else {
        body.push_str("## Saved events\n\n");
        for turn in turns {
            body.push_str(&format!(
                "- Turn {} — {}\n",
                turn.turn_number,
                recap_line(&turn.event.payload)
            ));
        }
    }
    if body.len() > MAX_PRIVATE_RECAP_BYTES {
        return invalid(
            "private campaign recap",
            "rendered-body",
            "recap exceeds its durable body limit",
        );
    }
    Ok(body)
}

fn recap_line(payload: &SessionEventPayload) -> String {
    match payload {
        SessionEventPayload::SessionStarted => "The campaign began.".to_owned(),
        SessionEventPayload::PlayerIntent { .. } => {
            "A player intent was committed; free-form wording is omitted from the recap.".to_owned()
        }
        SessionEventPayload::DiceResolved {
            purpose,
            total,
            modifier,
            ..
        } => format!(
            "The {} roll resolved to {total} with modifier {modifier}.",
            escape_markdown_line(purpose)
        ),
        SessionEventPayload::AbilityCheckResolved {
            action_id, result, ..
        } => format!(
            "The authored {} check {} with total {} against DC {}.",
            escape_markdown_line(action_id),
            if result.success { "succeeded" } else { "failed" },
            result.total,
            result.difficulty_class,
        ),
        SessionEventPayload::ExplorationSocialResolved { outcome, .. } => format!(
            "The authored lockkeeper conversation {} with total {} against DC {}; its objective, clock, and attitude changes were saved.",
            if outcome.check.result.outcome
                == manchester_dnd_core::rules_matrix::D20TestOutcome::Success
            {
                "succeeded"
            } else {
                "failed"
            },
            outcome.check.result.total,
            outcome.check.difficulty.difficulty_class,
        ),
        SessionEventPayload::EncounterResolved { outcome, .. } => {
            escape_markdown_line(&outcome.resolution.narration.authored_text)
        }
        SessionEventPayload::GmNarration {
            text,
            source_prompt_id,
            ..
        } if source_prompt_id.is_none() => escape_markdown_line(text),
        SessionEventPayload::GmNarration { .. } => {
            "A private-inspired presentation was committed; its wording is omitted because recap consent is independently scoped.".to_owned()
        }
        SessionEventPayload::ExperienceAwarded { summary, .. } => format!(
            "A trusted reward added {} XP, bringing the saved total to {} XP.",
            summary.awarded, summary.experience_points
        ),
        SessionEventPayload::AiProposalAccepted { .. } => {
            "A typed model proposal passed deterministic validation.".to_owned()
        }
        SessionEventPayload::AiProposalRejected { reason, .. } => format!(
            "A typed model proposal was rejected: {}.",
            escape_markdown_line(reason)
        ),
        SessionEventPayload::SessionEnded => "The campaign session ended.".to_owned(),
    }
}

fn escape_markdown_line(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.trim().chars().take(4_000) {
        match character {
            '\n' | '\r' | '\t' => escaped.push(' '),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '(' | ')' | '#' | '+'
            | '-' | '.' | '!' | '|' => {
                escaped.push('\\');
                escaped.push(character);
            }
            character if character.is_control() => escaped.push(' '),
            character => escaped.push(character),
        }
    }
    escaped
}

fn command_fingerprint(
    command: &GeneratePrivateRecapCommand,
) -> Result<Sha256Digest, RepositoryError> {
    let encoded = serde_json::to_vec(command).map_err(|source| RepositoryError::Serialize {
        entity: "private recap command",
        source,
    })?;
    Ok(digest(&encoded))
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

fn digest_from_row(row: &PgRow, column: &str, id: &str) -> Result<Sha256Digest, RepositoryError> {
    let value: String = row.try_get(column).map_err(RepositoryError::Database)?;
    Sha256Digest::new(value).map_err(|_| RepositoryError::InvalidDomainState {
        entity: "private campaign recap",
        id: id.to_owned(),
        reason: "stored digest is not canonical",
    })
}

fn validate_owner(owner_key: &str) -> Result<(), RepositoryError> {
    if is_valid_opaque_id(owner_key) {
        Ok(())
    } else {
        invalid("campaign owner", owner_key, "owner key is invalid")
    }
}

fn invalid<T>(entity: &'static str, id: &str, reason: &'static str) -> Result<T, RepositoryError> {
    Err(RepositoryError::InvalidDomainState {
        entity,
        id: id.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use manchester_dnd_core::{
        EventActor, RULESET, SessionDto, SessionEventDto, SessionEventPayload, SessionStatus,
    };
    use sqlx::PgPool;

    use super::*;
    use crate::repository::MIGRATOR;

    const OWNER: &str = "recap-owner";
    const CAMPAIGN: &str = "recap-campaign";

    async fn seed_campaign(pool: &PgPool) -> PostgresRepository {
        let repository = PostgresRepository::from_pool(pool.clone());
        let mut session = SessionDto {
            schema_version: SESSION_SCHEMA_VERSION,
            id: CAMPAIGN.to_owned(),
            ruleset: RULESET,
            title: "Rain *and* brass".to_owned(),
            status: SessionStatus::Active,
            character_ids: Vec::new(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
            last_event_sequence: 0,
        };
        repository.create_campaign(&session, &[]).await.unwrap();
        sqlx::query("UPDATE campaign_sessions SET owner_key = $2 WHERE id = $1")
            .bind(CAMPAIGN)
            .bind(OWNER)
            .execute(pool)
            .await
            .unwrap();
        session.updated_at_unix_ms = 2;
        session.last_event_sequence = 1;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: CAMPAIGN.to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 2,
            actor: EventActor::System,
            payload: SessionEventPayload::SessionStarted,
        };
        sqlx::query(
            "UPDATE campaign_sessions
             SET revision = 2, payload_json = $2::jsonb, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1",
        )
        .bind(CAMPAIGN)
        .bind(serde_json::to_string(&session).unwrap())
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO turn_audits
             (id, campaign_session_id, turn_number, schema_version, payload_json)
             VALUES ('recap-turn-1', $1, 1, 1, $2::jsonb)",
        )
        .bind(CAMPAIGN)
        .bind(serde_json::to_string(&event).unwrap())
        .execute(pool)
        .await
        .unwrap();
        repository
    }

    fn command(key: &str) -> GeneratePrivateRecapCommand {
        GeneratePrivateRecapCommand {
            schema_version: PRIVATE_RECAP_SCHEMA_VERSION,
            campaign_session_id: CAMPAIGN.to_owned(),
            expected_campaign_revision: 2,
            idempotency_key: key.to_owned(),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn recap_is_owner_scoped_audit_derived_idempotent_and_cascaded(pool: PgPool) {
        let repository = seed_campaign(&pool).await;
        let first = repository
            .generate_private_recap(OWNER, &command("recap-command-one"))
            .await
            .unwrap();
        assert!(first.body.contains("Turn 1 — The campaign began."));
        assert!(first.body.contains("Rain \\*and\\* brass"));
        assert_eq!(first.source_audit_count, 1);
        assert_eq!(
            repository
                .generate_private_recap(OWNER, &command("recap-command-one"))
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            repository
                .generate_private_recap(OWNER, &command("another-command-same-revision"))
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            repository
                .load_latest_private_recap(OWNER, CAMPAIGN)
                .await
                .unwrap(),
            Some(first)
        );
        assert!(matches!(
            repository
                .load_latest_private_recap("another-owner", CAMPAIGN)
                .await,
            Err(RepositoryError::NotFound { .. })
        ));

        sqlx::query("DELETE FROM campaign_sessions WHERE id = $1")
            .bind(CAMPAIGN)
            .execute(&pool)
            .await
            .unwrap();
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM campaign_private_recaps")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
