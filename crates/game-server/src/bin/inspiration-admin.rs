//! Offline local-operator workflow for private inspiration.
//!
//! The command file contains only opaque IDs, closed policy values, expiry
//! times, and cryptographic evidence digests. Raw Markdown is never accepted:
//! registration/review/grant operations resolve the exact already-screened
//! prompt loaded from the configured protected source root.

use std::{collections::BTreeSet, env, fs, process::ExitCode};

use manchester_dnd_core::Sha256Digest;
use manchester_dnd_server::{
    AppConfig, PrivateInspirationError, ServerContext,
    events::{EventPrompt, EventPromptLoadReview, EventPromptLoader},
    inspiration::{
        CampaignInspirationTone, ConfigureCampaignInspirationCommand, ConsentRevocationCode,
        DeleteParticipantPrivateDataCommand, DerivedArtifactPolicy, GrantConsentCommand,
        InspirationAudience, InspirationMedia, InspirationTransformation, OpaqueInspirationId,
        OpaqueOperatorId, PRIVATE_INSPIRATION_SCHEMA_VERSION, ParticipantVerificationMethod,
        PurgeExpiredParticipantDeletionTombstonesCommand, RecordRestrictedDiagnosticAccessCommand,
        RegisterSourceVersionCommand, RestrictedDiagnosticAccessKind, RestrictedDiagnosticDecision,
        RestrictedDiagnosticPurpose, ReviewSourceVersionCommand, RevokeConsentCommand,
        SafetySetupEvidence, SetGlobalInspirationControlCommand, SourceReviewState,
        VerifyParticipantCommand,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_COMMAND_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum AdminCommand {
    Status {
        campaign_session_id: String,
    },
    SetGlobalControl {
        idempotency_key: String,
        expected_revision: u64,
        generation_disabled: bool,
        operator_id: String,
        evidence_digest: String,
    },
    PurgeExpiredDeletionTombstones {
        idempotency_key: String,
        delete_after_epoch_inclusive: u64,
        operator_id: String,
        evidence_digest: String,
    },
    ConfigureCampaign {
        campaign_session_id: String,
        idempotency_key: String,
        expected_revision: u64,
        enabled: bool,
        evidence_digest: String,
        reviewer_id: String,
        tone: CampaignInspirationTone,
        #[serde(default)]
        allowed_sensitivity_codes: Vec<String>,
        #[serde(default)]
        line_codes: Vec<String>,
        #[serde(default)]
        veil_codes: Vec<String>,
        #[serde(default)]
        excluded_topic_codes: Vec<String>,
        #[serde(default)]
        excluded_participant_ids: Vec<String>,
    },
    VerifyParticipant {
        campaign_session_id: String,
        idempotency_key: String,
        participant_id: String,
        method: ParticipantVerificationMethod,
        evidence_digest: String,
        verifier_id: String,
    },
    RegisterLoadedSource {
        campaign_session_id: String,
        idempotency_key: String,
        source_id: String,
        source_version: u64,
        category_id: String,
        owner_participant_id: String,
        eligible_theme_pack_ids: Vec<String>,
        provenance_digest: String,
        expires_at_epoch: Option<u64>,
    },
    ReviewLoadedSource {
        campaign_session_id: String,
        idempotency_key: String,
        source_id: String,
        source_version: u64,
        decision: SourceReviewState,
        reviewer_id: String,
        review_evidence_digest: String,
    },
    GrantLoadedSourceConsent {
        campaign_session_id: String,
        idempotency_key: String,
        source_id: String,
        source_version: u64,
        participant_id: String,
        expires_at_epoch: u64,
        reviewer_id: String,
        participant_confirmation_digest: String,
        review_evidence_digest: String,
        artifact_policy: DerivedArtifactPolicy,
    },
    RevokeConsent {
        campaign_session_id: String,
        idempotency_key: String,
        grant_id: String,
        requester_participant_id: String,
        reason: ConsentRevocationCode,
    },
    ParticipantExport {
        campaign_session_id: String,
        requesting_participant_id: String,
    },
    DeleteParticipantData {
        campaign_session_id: String,
        idempotency_key: String,
        participant_id: String,
        operator_id: String,
        deletion_evidence_digest: String,
    },
    LoadedSourceInventory,
    RecordDiagnosticAccess {
        idempotency_key: String,
        campaign_session_id: Option<String>,
        operator_id: String,
        access_kind: RestrictedDiagnosticAccessKind,
        purpose: RestrictedDiagnosticPurpose,
        subject_id: String,
        evidence_digest: String,
        decision: RestrictedDiagnosticDecision,
    },
}

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&format!("runtime initialization failed: {error}")),
    };
    match runtime.block_on(run()) {
        Ok(value) => match serde_json::to_string_pretty(&json!({ "ok": value })) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(_) => fail("redacted response serialization failed"),
        },
        Err(error) => fail(&error),
    }
}

async fn run() -> Result<Value, String> {
    let mut args = env::args_os();
    let _program = args.next();
    let path = args.next().ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let metadata = fs::metadata(&path).map_err(|_| "command file is unavailable".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_COMMAND_BYTES {
        return Err("command file must be a regular file no larger than 64 KiB".to_owned());
    }
    let command: AdminCommand = serde_json::from_slice(
        &fs::read(path).map_err(|_| "command file could not be read".to_owned())?,
    )
    .map_err(|_| "command file does not match the closed admin schema".to_owned())?;
    let config = AppConfig::load().map_err(|error| error.to_string())?;
    // Only this offline operator process reads the protected source mount. The
    // ordinary server reconstructs minimized runtime prompts from MongoDB
    // and therefore needs neither this mount nor its decryption key.
    let event_review = if config.inspiration_enabled {
        EventPromptLoader
            .load_dir_reviewed(&config.event_prompts_dir)
            .map_err(|error| error.to_string())?
    } else {
        EventPromptLoadReview::default()
    };
    let context = ServerContext::from_config(config)
        .await
        .map_err(|error| error.to_string())?;
    execute(&context, &event_review.approved_prompts, command).await
}

async fn execute(
    context: &ServerContext,
    loaded_prompts: &[EventPrompt],
    command: AdminCommand,
) -> Result<Value, String> {
    match command {
        AdminCommand::Status {
            campaign_session_id,
        } => serialize(
            context
                .private_inspiration
                .campaign_status(&opaque(campaign_session_id)?)
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::SetGlobalControl {
            idempotency_key,
            expected_revision,
            generation_disabled,
            operator_id,
            evidence_digest,
        } => serialize(
            context
                .private_inspiration
                .set_global_control(SetGlobalInspirationControlCommand {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    idempotency_key: opaque(idempotency_key)?,
                    expected_revision,
                    generation_disabled,
                    operator_id: operator(operator_id)?,
                    evidence_digest: digest(evidence_digest)?,
                })
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::PurgeExpiredDeletionTombstones {
            idempotency_key,
            delete_after_epoch_inclusive,
            operator_id,
            evidence_digest,
        } => serialize(
            context
                .private_inspiration
                .purge_expired_deletion_tombstones(
                    PurgeExpiredParticipantDeletionTombstonesCommand {
                        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                        idempotency_key: opaque(idempotency_key)?,
                        delete_after_epoch_inclusive,
                        operator_id: operator(operator_id)?,
                        evidence_digest: digest(evidence_digest)?,
                    },
                )
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::ConfigureCampaign {
            campaign_session_id,
            idempotency_key,
            expected_revision,
            enabled,
            evidence_digest,
            reviewer_id,
            tone,
            allowed_sensitivity_codes,
            line_codes,
            veil_codes,
            excluded_topic_codes,
            excluded_participant_ids,
        } => serialize(
            context
                .private_inspiration
                .configure_campaign(ConfigureCampaignInspirationCommand {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    campaign_session_id: opaque(campaign_session_id)?,
                    idempotency_key: opaque(idempotency_key)?,
                    expected_revision,
                    enabled,
                    safety_setup: Some(SafetySetupEvidence {
                        evidence_digest: digest(evidence_digest)?,
                        reviewer_id: operator(reviewer_id)?,
                        tone,
                        allowed_sensitivity_codes: opaque_set(allowed_sensitivity_codes)?,
                        line_codes: opaque_set(line_codes)?,
                        veil_codes: opaque_set(veil_codes)?,
                        excluded_topic_codes: opaque_set(excluded_topic_codes)?,
                        excluded_participant_ids: opaque_set(excluded_participant_ids)?,
                    }),
                })
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::VerifyParticipant {
            campaign_session_id,
            idempotency_key,
            participant_id,
            method,
            evidence_digest,
            verifier_id,
        } => serialize(
            context
                .private_inspiration
                .verify_participant(VerifyParticipantCommand {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    campaign_session_id: opaque(campaign_session_id)?,
                    idempotency_key: opaque(idempotency_key)?,
                    participant_id: opaque(participant_id)?,
                    method,
                    evidence_digest: digest(evidence_digest)?,
                    verifier_id: operator(verifier_id)?,
                })
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::RegisterLoadedSource {
            campaign_session_id,
            idempotency_key,
            source_id,
            source_version,
            category_id,
            owner_participant_id,
            eligible_theme_pack_ids,
            provenance_digest,
            expires_at_epoch,
        } => {
            let prompt = loaded_prompt(loaded_prompts, &source_id)?;
            serialize(
                context
                    .private_inspiration
                    .register_source_version(RegisterSourceVersionCommand {
                        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                        campaign_session_id: opaque(campaign_session_id)?,
                        idempotency_key: opaque(idempotency_key)?,
                        source_id: opaque(source_id)?,
                        source_version,
                        source_digest: prompt.source_digest().clone(),
                        category_id: opaque(category_id)?,
                        owner_participant_id: opaque(owner_participant_id)?,
                        participant_ids: opaque_set(prompt.metadata.participant_aliases.clone())?,
                        sensitivity_codes: opaque_set(prompt.metadata.sensitivity_tags.clone())?,
                        eligible_media: BTreeSet::from([InspirationMedia::Text]),
                        eligible_theme_pack_ids: opaque_set(eligible_theme_pack_ids)?,
                        provenance_digest: digest(provenance_digest)?,
                        expires_at_epoch,
                        runtime_prompt: prompt.runtime_projection(),
                    })
                    .await
                    .map_err(private_error)?,
            )
        }
        AdminCommand::ReviewLoadedSource {
            campaign_session_id,
            idempotency_key,
            source_id,
            source_version,
            decision,
            reviewer_id,
            review_evidence_digest,
        } => {
            let prompt = loaded_prompt(loaded_prompts, &source_id)?;
            serialize(
                context
                    .private_inspiration
                    .review_source_version(ReviewSourceVersionCommand {
                        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                        campaign_session_id: opaque(campaign_session_id)?,
                        idempotency_key: opaque(idempotency_key)?,
                        source_id: opaque(source_id)?,
                        source_version,
                        source_digest: prompt.source_digest().clone(),
                        decision,
                        q11_screened: decision == SourceReviewState::Approved,
                        reviewer_id: operator(reviewer_id)?,
                        review_evidence_digest: digest(review_evidence_digest)?,
                    })
                    .await
                    .map_err(private_error)?,
            )
        }
        AdminCommand::GrantLoadedSourceConsent {
            campaign_session_id,
            idempotency_key,
            source_id,
            source_version,
            participant_id,
            expires_at_epoch,
            reviewer_id,
            participant_confirmation_digest,
            review_evidence_digest,
            artifact_policy,
        } => {
            let prompt = loaded_prompt(loaded_prompts, &source_id)?;
            serialize(
                context
                    .private_inspiration
                    .grant_consent(GrantConsentCommand {
                        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                        campaign_session_id: opaque(campaign_session_id)?,
                        idempotency_key: opaque(idempotency_key)?,
                        source_id: opaque(source_id)?,
                        source_version,
                        source_digest: prompt.source_digest().clone(),
                        participant_id: opaque(participant_id)?,
                        audience: InspirationAudience::PrivateCampaign,
                        media: InspirationMedia::Text,
                        transformation: InspirationTransformation::HighFictionDistanceV1,
                        sensitivity_codes: opaque_set(prompt.metadata.sensitivity_tags.clone())?,
                        expires_at_epoch,
                        reviewer_id: operator(reviewer_id)?,
                        participant_confirmation_digest: digest(participant_confirmation_digest)?,
                        review_evidence_digest: digest(review_evidence_digest)?,
                        artifact_policy,
                    })
                    .await
                    .map_err(private_error)?,
            )
        }
        AdminCommand::RevokeConsent {
            campaign_session_id,
            idempotency_key,
            grant_id,
            requester_participant_id,
            reason,
        } => serialize(
            context
                .private_inspiration
                .revoke_consent(RevokeConsentCommand {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    campaign_session_id: opaque(campaign_session_id)?,
                    idempotency_key: opaque(idempotency_key)?,
                    grant_id: opaque(grant_id)?,
                    requester_participant_id: opaque(requester_participant_id)?,
                    reason,
                })
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::ParticipantExport {
            campaign_session_id,
            requesting_participant_id,
        } => serialize(
            context
                .private_inspiration
                .redacted_export(
                    &opaque(campaign_session_id)?,
                    &opaque(requesting_participant_id)?,
                )
                .await
                .map_err(private_error)?,
        ),
        AdminCommand::DeleteParticipantData {
            campaign_session_id,
            idempotency_key,
            participant_id,
            operator_id,
            deletion_evidence_digest,
        } => {
            if loaded_prompts.iter().any(|prompt| {
                prompt
                    .metadata
                    .participant_aliases
                    .iter()
                    .any(|alias| alias == &participant_id)
            }) {
                return Err(
                    "a configured protected source still contains this participant; remove it and restart this command"
                        .to_owned(),
                );
            }
            serialize(
                context
                    .private_inspiration
                    .delete_participant_private_data(DeleteParticipantPrivateDataCommand {
                        schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                        campaign_session_id: opaque(campaign_session_id)?,
                        idempotency_key: opaque(idempotency_key)?,
                        participant_id: opaque(participant_id)?,
                        operator_id: operator(operator_id)?,
                        deletion_evidence_digest: digest(deletion_evidence_digest)?,
                        protected_sources_removed: true,
                    })
                    .await
                    .map_err(private_error)?,
            )
        }
        AdminCommand::LoadedSourceInventory => serialize(
            loaded_prompts
                .iter()
                .map(|prompt| {
                    json!({
                        "source_id": prompt.privacy_source_id(),
                        "source_digest": prompt.source_digest(),
                        "schema_version": prompt.metadata.schema_version,
                        "participant_count": prompt.metadata.participant_aliases.len(),
                        "sensitivity_count": prompt.metadata.sensitivity_tags.len(),
                        "enabled": prompt.metadata.enabled,
                    })
                })
                .collect::<Vec<_>>(),
        ),
        AdminCommand::RecordDiagnosticAccess {
            idempotency_key,
            campaign_session_id,
            operator_id,
            access_kind,
            purpose,
            subject_id,
            evidence_digest,
            decision,
        } => serialize(
            context
                .private_inspiration
                .record_restricted_diagnostic_access(RecordRestrictedDiagnosticAccessCommand {
                    schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                    idempotency_key: opaque(idempotency_key)?,
                    campaign_session_id: campaign_session_id.map(opaque).transpose()?,
                    operator_id: operator(operator_id)?,
                    access_kind,
                    purpose,
                    subject_id: opaque(subject_id)?,
                    evidence_digest: digest(evidence_digest)?,
                    decision,
                })
                .await
                .map_err(private_error)?,
        ),
    }
}

fn loaded_prompt<'a>(
    loaded_prompts: &'a [EventPrompt],
    source_id: &str,
) -> Result<&'a EventPrompt, String> {
    loaded_prompts
        .iter()
        .find(|prompt| prompt.privacy_source_id() == source_id)
        .ok_or_else(|| "approved loaded source was not found".to_owned())
}

fn opaque(value: String) -> Result<OpaqueInspirationId, String> {
    OpaqueInspirationId::new(value).map_err(private_error)
}

fn operator(value: String) -> Result<OpaqueOperatorId, String> {
    OpaqueOperatorId::new(value).map_err(private_error)
}

fn digest(value: String) -> Result<Sha256Digest, String> {
    Sha256Digest::new(value).map_err(|_| "a digest is not canonical SHA-256".to_owned())
}

fn opaque_set(values: Vec<String>) -> Result<BTreeSet<OpaqueInspirationId>, String> {
    values.into_iter().map(opaque).collect()
}

fn serialize(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "redacted response serialization failed".to_owned())
}

fn private_error(error: PrivateInspirationError) -> String {
    format!("{}: {}", error.public_code(), error.safe_message())
}

fn usage() -> String {
    "usage: inspiration-admin <closed-command.json>".to_owned()
}

fn fail(message: &str) -> ExitCode {
    let body = serde_json::to_string_pretty(&json!({
        "ok": false,
        "error": message,
    }))
    .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"command failed\"}".to_owned());
    eprintln!("{body}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_command_schema_rejects_raw_source_fields() {
        let body = r#"{
          "operation":"delete_participant_data",
          "campaign_session_id":"campaign:test",
          "idempotency_key":"deletion:test",
          "participant_id":"participant:11111111111111111111111111111111",
          "operator_id":"operator:22222222222222222222222222222222",
          "deletion_evidence_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "raw_markdown":"must never cross this boundary"
        }"#;
        assert!(serde_json::from_str::<AdminCommand>(body).is_err());
    }

    #[test]
    fn closed_deletion_command_contains_only_policy_evidence() {
        let body = r#"{
          "operation":"delete_participant_data",
          "campaign_session_id":"campaign:test",
          "idempotency_key":"deletion:test",
          "participant_id":"participant:11111111111111111111111111111111",
          "operator_id":"operator:22222222222222222222222222222222",
          "deletion_evidence_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }"#;
        assert!(serde_json::from_str::<AdminCommand>(body).is_ok());
    }
}
