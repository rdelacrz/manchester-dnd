use leptos::task::spawn_local;
use leptos::{prelude::*, server_fn::codec::Json};
use manchester_dnd_core::{CommittedEncounterOutcomeDto, LocalCampaignViewDto};
use serde::{Deserialize, Serialize};

use crate::campaign::PublicGameError;

pub const TYPED_INTENT_COMMAND_SCHEMA_VERSION: u16 = 1;
pub const NARRATION_REGENERATION_SCHEMA_VERSION: u16 = 1;
pub const PRIVATE_INSPIRATION_REDACTION_NOTICE: &str = "Private inspiration removed at a participant request. The committed game mechanics are unchanged.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedIntentCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_campaign_revision: u64,
    pub expected_encounter_revision: u64,
    pub idempotency_key: String,
    pub player_intent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub source: String,
    pub failure: Option<String>,
    pub attempts: u8,
    pub prompt_fingerprint: String,
    pub policy_fingerprint: String,
    pub config_fingerprint: String,
    pub proposal_fingerprint: String,
    pub model: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationChoiceView {
    pub choice_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrationPresentationView {
    pub presentation_id: String,
    pub version: u8,
    pub selected: bool,
    pub source: String,
    pub private_inspiration_used: bool,
    pub privacy_redacted: bool,
    pub body: String,
    pub generation_job_id: String,
    pub generation_attempt_id: String,
    pub config_digest: String,
    pub prompt_digest: String,
    pub policy_digest: String,
    pub output_digest: String,
    pub retention_delete_after: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedTypedIntent {
    pub outcome: CommittedEncounterOutcomeDto,
    pub interpretation: String,
    pub narration: String,
    pub interpretation_evidence: GenerationEvidence,
    pub narration_evidence: Option<GenerationEvidence>,
    pub narration_versions: Vec<NarrationPresentationView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resolution", content = "payload", rename_all = "snake_case")]
pub enum TypedIntentResolution {
    Committed(Box<CommittedTypedIntent>),
    Clarification {
        question: String,
        choices: Vec<ClarificationChoiceView>,
        evidence: GenerationEvidence,
    },
    Degraded {
        message: String,
        authored_alternatives: Vec<String>,
        evidence: GenerationEvidence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum TypedIntentResponse {
    Resolved(Box<TypedIntentResolution>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegenerateNarrationCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub event_sequence: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegeneratedNarration {
    pub event_sequence: u64,
    pub presentation_id: String,
    pub presentation_version: u8,
    pub requested_presentation_selected: bool,
    pub selected_presentation_version: Option<u8>,
    pub narration: String,
    pub evidence: GenerationEvidence,
    pub versions: Vec<NarrationPresentationView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum RegenerateNarrationResponse {
    Regenerated(Box<RegeneratedNarration>),
    Rejected(PublicGameError),
}

/// Signals owned by the stable encounter panel rather than its reactive
/// campaign-view branch. Committing mechanics replaces that branch; keeping
/// these signals outside it preserves the presentation result and retry key.
#[derive(Clone, Copy)]
pub struct FreeformIntentState {
    intent: RwSignal<String>,
    result: RwSignal<Option<TypedIntentResolution>>,
    intent_retry_command: RwSignal<Option<TypedIntentCommand>>,
    retry_command: RwSignal<Option<RegenerateNarrationCommand>>,
}

impl FreeformIntentState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            intent: RwSignal::new(String::new()),
            result: RwSignal::new(None),
            intent_retry_command: RwSignal::new(None),
            retry_command: RwSignal::new(None),
        }
    }

    pub fn current_private_presentation_id(self) -> Option<String> {
        let result = self.result.get()?;
        let TypedIntentResolution::Committed(committed) = result else {
            return None;
        };
        committed
            .narration_versions
            .iter()
            .find(|version| {
                version.selected && version.private_inspiration_used && !version.privacy_redacted
            })
            .map(|version| version.presentation_id.clone())
    }

    /// Immediately removes the current source-derived prose from the live DOM
    /// after the server has durably accepted a privacy intervention.
    pub fn hide_private_presentation(self, presentation_id: &str, use_engine_fallback: bool) {
        self.result.update(|result| {
            let Some(TypedIntentResolution::Committed(committed)) = result else {
                return;
            };
            let selected_match = committed
                .narration_versions
                .iter()
                .any(|version| version.presentation_id == presentation_id && version.selected);
            if !selected_match {
                return;
            }
            if use_engine_fallback {
                committed.narration = committed.outcome.resolution.narration.authored_text.clone();
                committed
                    .narration_versions
                    .retain(|version| version.presentation_id != presentation_id);
            } else {
                committed.narration = PRIVATE_INSPIRATION_REDACTION_NOTICE.to_owned();
                if let Some(version) = committed
                    .narration_versions
                    .iter_mut()
                    .find(|version| version.presentation_id == presentation_id)
                {
                    version.body = PRIVATE_INSPIRATION_REDACTION_NOTICE.to_owned();
                    version.privacy_redacted = true;
                }
            }
        });
    }
}

impl Default for FreeformIntentState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "ssr")]
#[derive(Clone, Copy)]
struct NarrationGenerationOrigin<'a> {
    campaign_session_id: &'a str,
    campaign_revision: u64,
    event_sequence: u64,
    idempotency_key: &'a str,
    correlation_id: &'a str,
    engine_narration: &'a str,
    private_inspiration_work_id: Option<&'a str>,
}

#[cfg(feature = "ssr")]
struct RecordedNarration {
    presentation_id: String,
    presentation_version: u8,
    requested_presentation_selected: bool,
    selected_presentation_version: Option<u8>,
    narration: String,
    evidence: GenerationEvidence,
    versions: Vec<NarrationPresentationView>,
}

#[cfg(feature = "ssr")]
enum RecordedNarrationReplay {
    Missing,
    Available(Box<RecordedNarration>),
    Expired {
        versions: Vec<NarrationPresentationView>,
    },
}

#[server(input = Json)]
pub async fn submit_typed_player_intent(
    command: TypedIntentCommand,
) -> Result<TypedIntentResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use std::collections::{BTreeMap, BTreeSet};

        use manchester_dnd_core::{
            CommitEncounterCommand, ENCOUNTER_COMMIT_SCHEMA_VERSION,
            ai_turn::{MechanicalFact, ProposalAcceptanceContext, TypedGmProposal},
            encounter::{
                EncounterCommand, EncounterFact, EncounterIntent, EncounterRollPurpose,
                EncounterStatus, LegalEncounterAction, LifeStatus, RawRollOutcome,
                RollComparisonKind,
            },
            hero::SpellId,
            is_valid_opaque_id,
        };
        use manchester_dnd_server::{
            ApplicationError, InlineGenerationRequest, ServerContext,
            generation_ledger::PendingTypedIntentCommandRequest,
            repository::jobs::GenerationPurpose,
            typed_gm::{
                AudiencePolicy, CommittedPublicFact, GmPromptPolicy, GmSafetyPolicy, GmThemePolicy,
                PrivateInspirationPolicy, SafetyCategory, TYPED_GM_PROMPT_TEMPLATE_ID,
                TypedGmPurpose, TypedGmTurnInput,
            },
        };

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        if command.schema_version != TYPED_INTENT_COMMAND_SCHEMA_VERSION
            || !is_valid_opaque_id(&command.campaign_session_id)
            || !is_valid_opaque_id(&command.idempotency_key)
            || command.expected_campaign_revision == 0
            || command.expected_encounter_revision == 0
            || command.player_intent.trim().is_empty()
            || command.player_intent.trim().chars().count() > 4_000
        {
            return Ok(TypedIntentResponse::Rejected(typed_public_error(
                "invalid_typed_intent",
                "Describe one bounded action using between 1 and 4,000 characters.",
                false,
                correlation_id,
            )));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };

        let view = match context.application.load_local_campaign().await {
            Ok(view) => view,
            Err(error) => {
                return Ok(TypedIntentResponse::Rejected(
                    crate::campaign::public_error(&error, correlation_id),
                ));
            }
        };
        if command.campaign_session_id != view.campaign_session_id {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::public_error(&ApplicationError::WrongCampaign, correlation_id),
            ));
        }
        match context
            .generation_ledger
            .typed_intent_command_receipt(
                &command.campaign_session_id,
                &command.idempotency_key,
                &command.player_intent,
            )
            .await
        {
            Ok(Some(receipt)) => {
                return Ok(replay_committed_typed_intent(
                    &context,
                    &command,
                    receipt,
                    correlation_id,
                )
                .await);
            }
            Ok(None) => {}
            Err(manchester_dnd_server::GenerationLedgerError::TypedIntentIdempotencyConflict) => {
                return Ok(TypedIntentResponse::Rejected(public_retry_error(
                    "idempotency_conflict",
                    "That request key is already bound to different player intent text.",
                    false,
                    correlation_id,
                )));
            }
            Err(_) => {
                return Ok(TypedIntentResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                ));
            }
        }
        if command.expected_campaign_revision != view.revision {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::public_error(
                    &ApplicationError::RevisionConflict {
                        expected: command.expected_campaign_revision,
                        current_revision: view.revision,
                    },
                    correlation_id,
                ),
            ));
        }
        let Some(encounter) = view.encounter.as_ref() else {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::public_error(
                    &ApplicationError::EncounterUnavailable,
                    correlation_id,
                ),
            ));
        };
        if command.expected_encounter_revision != encounter.state.revision {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::public_error(
                    &ApplicationError::EncounterRevisionConflict {
                        expected: command.expected_encounter_revision,
                        current_revision: encounter.state.revision,
                    },
                    correlation_id,
                ),
            ));
        }
        if encounter.state.status == EncounterStatus::Active
            && encounter.state.current_actor_id.as_deref() != Some(encounter.state.hero.id.as_str())
        {
            return Ok(TypedIntentResponse::Rejected(typed_public_error(
                "not_player_turn",
                "The creature turn is resolved by the deterministic server policy; player intent cannot select its action.",
                false,
                correlation_id,
            )));
        }

        let (action_map, action_facts) =
            encounter_action_map(&encounter.state, &encounter.legal_actions);
        if action_map.is_empty() {
            return Ok(TypedIntentResponse::Rejected(typed_public_error(
                "no_legal_actions",
                "The encounter has no action available for interpretation.",
                false,
                correlation_id,
            )));
        }
        let mut policy = prompt_policy(&context).await;
        let fingerprints = match context.typed_game_master.fingerprints(&policy) {
            Ok(fingerprints) => fingerprints,
            Err(_) => {
                return Ok(TypedIntentResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                ));
            }
        };
        let target_ids = encounter
            .legal_actions
            .iter()
            .filter_map(|action| match action {
                LegalEncounterAction::Attack { target_id, .. }
                | LegalEncounterAction::CastSpell { target_id, .. } => Some(target_id.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let acceptance = ProposalAcceptanceContext {
            session_id: view.campaign_session_id.clone(),
            revision: view.revision,
            event_sequence: view.last_event_sequence,
            prompt_template_id: TYPED_GM_PROMPT_TEMPLATE_ID.to_owned(),
            policy_id: policy.policy_id.clone(),
            config_fingerprint: fingerprints.config,
            legal_action_ids: action_map.keys().cloned().collect(),
            legal_check_ids: BTreeSet::new(),
            legal_target_ids: target_ids,
            legal_scene_ids: BTreeSet::new(),
            legal_objective_ids: BTreeSet::from([
                encounter.state.objectives.primary.objective_id.clone(),
                encounter.state.objectives.contextual.objective_id.clone(),
            ]),
            authoritative_facts: Vec::new(),
        };
        let mut public_facts = action_facts;
        public_facts.extend(encounter_public_facts(&encounter.state));
        let input = TypedGmTurnInput {
            purpose: TypedGmPurpose::InterpretPlayerIntent,
            acceptance,
            public_facts,
            player_intent: Some(command.player_intent.trim().to_owned()),
            private_inspiration: None,
            policy: policy.clone(),
        };
        let prepared = match context.typed_game_master.prepare_request(&input) {
            Ok(prepared) => prepared,
            Err(_) => {
                return Ok(TypedIntentResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                ));
            }
        };
        let interpretation_attempt = match context
            .generation_ledger
            .begin(InlineGenerationRequest {
                campaign_session_id: view.campaign_session_id.clone(),
                origin_turn_id: None,
                origin_campaign_revision: view.revision,
                purpose: GenerationPurpose::IntentParsing,
                idempotency_key: format!("typed-intent:{}", command.idempotency_key),
                input_digest: prepared.request_fingerprint,
                prompt_digest: prepared.fingerprints.prompt,
                policy_digest: prepared.fingerprints.policy,
                config_digest: prepared.fingerprints.config,
                correlation_id: correlation_id.clone(),
            })
            .await
        {
            Ok(attempt) => attempt,
            Err(manchester_dnd_server::GenerationLedgerError::AlreadyHandled { .. })
            | Err(manchester_dnd_server::GenerationLedgerError::ExactJobUnavailable) => {
                return Ok(TypedIntentResponse::Rejected(typed_public_error(
                    "generation_already_handled",
                    "This generation request was already handled or is still running. Reload the authoritative campaign before trying a new intent.",
                    false,
                    correlation_id,
                )));
            }
            Err(manchester_dnd_server::GenerationLedgerError::Store(
                manchester_dnd_server::repository::jobs::GenerationJobStoreError::IdempotencyConflict,
            )) => {
                return Ok(TypedIntentResponse::Rejected(typed_public_error(
                    "idempotency_conflict",
                    "This request key was already used for a different player intent. Keep the original request unchanged or start a new action.",
                    false,
                    correlation_id,
                )));
            }
            Err(manchester_dnd_server::GenerationLedgerError::Store(
                manchester_dnd_server::repository::jobs::GenerationJobStoreError::BudgetExceeded {
                    ..
                },
            )) => {
                return Ok(TypedIntentResponse::Rejected(typed_public_error(
                    "generation_budget_exceeded",
                    "The campaign's AI generation budget is exhausted. Structured authored actions and deterministic game text remain available.",
                    false,
                    correlation_id,
                )));
            }
            Err(manchester_dnd_server::GenerationLedgerError::Store(_))
            | Err(manchester_dnd_server::GenerationLedgerError::Repository(_))
            | Err(manchester_dnd_server::GenerationLedgerError::OriginTurnUnavailable)
            | Err(manchester_dnd_server::GenerationLedgerError::OriginOutcomeUnavailable)
            | Err(manchester_dnd_server::GenerationLedgerError::Presentation(_))
            | Err(manchester_dnd_server::GenerationLedgerError::InvalidPresentation)
            | Err(manchester_dnd_server::GenerationLedgerError::TypedIntentIdempotencyConflict) => {
                return Ok(TypedIntentResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                ));
            }
        };
        let generated = match context.typed_game_master.generate(&input).await {
            Ok(result) => result,
            Err(_) => {
                let _ = context
                    .generation_ledger
                    .finish_unavailable(&interpretation_attempt)
                    .await;
                return Ok(TypedIntentResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                ));
            }
        };
        if context
            .generation_ledger
            .finish_typed(&interpretation_attempt, &generated)
            .await
            .is_err()
        {
            return Ok(TypedIntentResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        }
        let interpretation_evidence =
            narration_server::generation_evidence(&generated, Some(&interpretation_attempt));

        let resolution = match generated.proposal.clone() {
            TypedGmProposal::Action(proposal) => {
                let Some((intent, label)) = action_map.get(&proposal.action_id) else {
                    return Ok(TypedIntentResponse::Rejected(typed_public_error(
                        "unsupported_mechanic",
                        "That interpretation is outside the current legal action set.",
                        false,
                        correlation_id,
                    )));
                };
                let interpretation_evidence_json = match serde_json::to_string(&interpretation_evidence) {
                    Ok(value) => value,
                    Err(_) => {
                        return Ok(TypedIntentResponse::Rejected(
                            crate::campaign::internal_error(correlation_id),
                        ));
                    }
                };
                let pending_receipt = match context
                    .generation_ledger
                    .insert_pending_typed_intent_command_receipt(
                        PendingTypedIntentCommandRequest {
                            campaign_session_id: view.campaign_session_id.clone(),
                            client_idempotency_key: command.idempotency_key.clone(),
                            player_intent: command.player_intent.clone(),
                            expected_campaign_revision: view.revision,
                            expected_encounter_revision: encounter.state.revision,
                            resolved_intent: intent.clone(),
                            interpretation_label: label.clone(),
                            interpretation_evidence_json,
                        },
                    )
                    .await
                {
                    Ok(receipt) => receipt,
                    Err(manchester_dnd_server::GenerationLedgerError::TypedIntentIdempotencyConflict)
                    | Err(manchester_dnd_server::GenerationLedgerError::Presentation(
                        manchester_dnd_server::repository::TextPresentationStoreError::IdempotencyConflict,
                    )) => {
                        return Ok(TypedIntentResponse::Rejected(public_retry_error(
                            "idempotency_conflict",
                            "That request key is already bound to different player intent text.",
                            false,
                            correlation_id,
                        )));
                    }
                    Err(_) => {
                        return Ok(TypedIntentResponse::Rejected(
                            crate::campaign::internal_error(correlation_id),
                        ));
                    }
                };
                let committed = match context
                    .application
                    .commit_encounter_command_with_correlation(
                        CommitEncounterCommand {
                            schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
                            campaign_session_id: pending_receipt.campaign_session_id.clone(),
                            expected_campaign_revision: pending_receipt.expected_campaign_revision,
                            command: EncounterCommand::new(
                                pending_receipt.expected_encounter_revision,
                                pending_receipt.client_idempotency_key.clone(),
                                pending_receipt.resolved_intent.clone(),
                            ),
                        },
                        &correlation_id,
                    )
                    .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        return Ok(TypedIntentResponse::Rejected(
                            crate::campaign::public_error(&error, correlation_id),
                        ));
                    }
                };
                if context
                    .generation_ledger
                    .commit_typed_intent_command_receipt(
                        &pending_receipt,
                        &command.player_intent,
                        committed.event_sequence,
                        committed.result_campaign_revision,
                    )
                    .await
                    .is_err()
                {
                    return Ok(TypedIntentResponse::Rejected(
                        crate::campaign::internal_error(correlation_id),
                    ));
                }

                // Mechanics are already durable. Any narration failure below is
                // presentation-only and can never roll back or reroll the turn.
                let authoritative_facts = narration_facts(&committed);
                let narration_public_facts = vec![CommittedPublicFact {
                    fact_id: "fact:committed-engine-narration".to_owned(),
                    summary: committed.resolution.narration.authored_text.clone(),
                }];
                let reserved_inspiration =
                    narration_server::reserve_narration_inspiration(&context, &committed).await;
                let (private_inspiration, private_inspiration_work_id) =
                    match reserved_inspiration {
                        Some(reserved) => {
                            policy.safety.private_inspiration =
                                PrivateInspirationPolicy::MinimizedHighDistanceV1;
                            policy.theme.tone_tags.push(match reserved.tone {
                                narration_server::CampaignSafetyTone::Gothic => {
                                    "gothic-adventure"
                                }
                                narration_server::CampaignSafetyTone::Hopeful => {
                                    "hopeful-adventure"
                                }
                                narration_server::CampaignSafetyTone::Lighthearted => {
                                    "lighthearted-adventure"
                                }
                            }
                            .to_owned());
                            (Some(reserved.brief), Some(reserved.work_id))
                        }
                        None => (None, None),
                    };
                let narration_fingerprints = context
                    .typed_game_master
                    .fingerprints(&policy)
                    .expect("the already-validated policy remains valid");
                let narration_input = TypedGmTurnInput {
                    purpose: TypedGmPurpose::NarrateCommittedFacts,
                    acceptance: ProposalAcceptanceContext {
                        session_id: committed.campaign_session_id.clone(),
                        revision: committed.result_campaign_revision,
                        event_sequence: committed.event_sequence,
                        prompt_template_id: TYPED_GM_PROMPT_TEMPLATE_ID.to_owned(),
                        policy_id: policy.policy_id.clone(),
                        config_fingerprint: narration_fingerprints.config,
                        legal_action_ids: BTreeSet::new(),
                        legal_check_ids: BTreeSet::new(),
                        legal_target_ids: BTreeSet::new(),
                        legal_scene_ids: BTreeSet::new(),
                        legal_objective_ids: BTreeSet::new(),
                        authoritative_facts,
                    },
                    public_facts: narration_public_facts,
                    player_intent: None,
                    private_inspiration,
                    policy,
                };
                let engine_narration = committed.resolution.narration.authored_text.clone();
                let generated_narration = narration_server::generate_recorded_narration(
                    &context,
                    &narration_input,
                    NarrationGenerationOrigin {
                        campaign_session_id: &committed.campaign_session_id,
                        campaign_revision: committed.result_campaign_revision,
                        event_sequence: committed.event_sequence,
                        idempotency_key: &command.idempotency_key,
                        correlation_id: &correlation_id,
                        engine_narration: &engine_narration,
                        private_inspiration_work_id: private_inspiration_work_id.as_deref(),
                    },
                )
                .await;
                let (narration, narration_evidence, narration_versions) = match generated_narration {
                    Some(recorded) => (
                        recorded.narration,
                        Some(recorded.evidence),
                        recorded.versions,
                    ),
                    None => (
                        engine_narration,
                        None,
                        Vec::new(),
                    ),
                };
                TypedIntentResolution::Committed(Box::new(CommittedTypedIntent {
                    outcome: committed,
                    interpretation: format!(
                        "Matched the description to the authored legal action: {}.",
                        pending_receipt.interpretation_label
                    ),
                    narration,
                    interpretation_evidence,
                    narration_evidence,
                    narration_versions,
                }))
            }
            TypedGmProposal::Clarification(_) => {
                TypedIntentResolution::Clarification {
                    question: "Which currently authored legal action did you mean?".to_owned(),
                    choices: action_map
                        .iter()
                        .take(4)
                        .map(|(action_id, (_, label))| ClarificationChoiceView {
                            choice_id: action_id.clone(),
                            label: label.clone(),
                        })
                        .collect(),
                    evidence: interpretation_evidence,
                }
            }
            TypedGmProposal::Narration(_) => TypedIntentResolution::Degraded {
                message: "The intent did not resolve to one legal mechanic. The committed state is unchanged; choose an authored action or restate the intent.".to_owned(),
                authored_alternatives: action_map
                    .values()
                    .map(|(_, label)| label.clone())
                    .collect(),
                evidence: interpretation_evidence,
            },
            // Check and scene allowlists are empty for this encounter. Core
            // acceptance should make these unreachable; retain a safe fallback.
            TypedGmProposal::Check(_) | TypedGmProposal::Scene(_) => {
                TypedIntentResolution::Degraded {
                    message: "That intent needs a mechanic not offered in this encounter. Choose an authored legal action.".to_owned(),
                    authored_alternatives: action_map
                        .values()
                        .map(|(_, label)| label.clone())
                        .collect(),
                    evidence: interpretation_evidence,
                }
            }
        };
        return Ok(TypedIntentResponse::Resolved(Box::new(resolution)));

        fn typed_public_error(
            code: &str,
            message: &str,
            retryable: bool,
            correlation_id: String,
        ) -> PublicGameError {
            PublicGameError {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable,
                current_revision: None,
                correlation_id,
                alternatives: Vec::new(),
            }
        }

        fn encounter_action_map(
            state: &manchester_dnd_core::encounter::EncounterState,
            legal_actions: &[LegalEncounterAction],
        ) -> (
            BTreeMap<String, (EncounterIntent, String)>,
            Vec<CommittedPublicFact>,
        ) {
            let mut actions = BTreeMap::new();
            for action in legal_actions {
                match action {
                    LegalEncounterAction::Move {
                        minimum_destination_feet,
                        maximum_destination_feet,
                        ..
                    } => {
                        for destination in (*minimum_destination_feet..=*maximum_destination_feet)
                            .filter(|feet| feet.is_multiple_of(5))
                        {
                            let id = format!("encounter-action:move:{destination:03}");
                            actions.insert(
                                id,
                                (
                                    EncounterIntent::Move {
                                        destination_feet: destination,
                                    },
                                    format!("Move to {destination} feet"),
                                ),
                            );
                        }
                    }
                    LegalEncounterAction::StartEncounter => {
                        actions.insert(
                            "encounter-action:start".to_owned(),
                            (
                                EncounterIntent::StartEncounter,
                                "Roll initiative and begin".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::Attack {
                        attack_id,
                        target_id,
                        ..
                    } => {
                        let id = format!("encounter-action:attack:{}", actions.len());
                        let target = if target_id == &state.hero.id {
                            &state.hero.name
                        } else {
                            &state.creature.name
                        };
                        actions.insert(
                            id,
                            (
                                EncounterIntent::Attack {
                                    attack_id: attack_id.clone(),
                                    target_id: target_id.clone(),
                                },
                                format!("Attack {target}"),
                            ),
                        );
                    }
                    LegalEncounterAction::ContextAction { action_id } => {
                        actions.insert(
                            "encounter-action:context".to_owned(),
                            (
                                EncounterIntent::ContextAction {
                                    action_id: action_id.clone(),
                                },
                                "Release the sluice gate".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::CastSpell {
                        spell, target_id, ..
                    } => {
                        let (id, label) = match spell {
                            SpellId::FireBolt => (
                                "encounter-action:cast:fire-bolt",
                                "Cast Fire Bolt at the Soot Wight",
                            ),
                            SpellId::MagicMissile => (
                                "encounter-action:cast:magic-missile",
                                "Cast Magic Missile at the Soot Wight",
                            ),
                            SpellId::Light
                            | SpellId::MageHand
                            | SpellId::Shield
                            | SpellId::Sleep => continue,
                        };
                        actions.insert(
                            id.to_owned(),
                            (
                                EncounterIntent::CastSpell {
                                    spell: *spell,
                                    target_id: target_id.clone(),
                                },
                                label.to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::CastLight { object_id } => {
                        actions.insert(
                            "encounter-action:cast:light".to_owned(),
                            (
                                EncounterIntent::CastLight {
                                    object_id: object_id.clone(),
                                },
                                format!(
                                    "Cast Light on {}",
                                    super::authored_object_label(object_id)
                                ),
                            ),
                        );
                    }
                    LegalEncounterAction::CastMageHand { anchor_object_id } => {
                        actions.insert(
                            "encounter-action:cast:mage-hand".to_owned(),
                            (
                                EncounterIntent::CastMageHand {
                                    anchor_object_id: anchor_object_id.clone(),
                                },
                                format!(
                                    "Cast Mage Hand by {}",
                                    super::authored_object_label(anchor_object_id)
                                ),
                            ),
                        );
                    }
                    LegalEncounterAction::ControlMageHand { object_id } => {
                        actions.insert(
                            "encounter-action:control:mage-hand".to_owned(),
                            (
                                EncounterIntent::ControlMageHand {
                                    object_id: object_id.clone(),
                                },
                                format!(
                                    "Use Mage Hand on {}",
                                    super::authored_object_label(object_id)
                                ),
                            ),
                        );
                    }
                    LegalEncounterAction::DismissMageHand => {
                        actions.insert(
                            "encounter-action:dismiss:mage-hand".to_owned(),
                            (
                                EncounterIntent::DismissMageHand,
                                "Dismiss Mage Hand".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::CastSleep => {
                        actions.insert(
                            "encounter-action:cast:sleep".to_owned(),
                            (EncounterIntent::CastSleep, "Cast Sleep".to_owned()),
                        );
                    }
                    LegalEncounterAction::CastShield => {
                        actions.insert(
                            "encounter-action:reaction:shield".to_owned(),
                            (EncounterIntent::CastShield, "React with Shield".to_owned()),
                        );
                    }
                    LegalEncounterAction::DeclineReaction => {
                        actions.insert(
                            "encounter-action:reaction:decline".to_owned(),
                            (
                                EncounterIntent::DeclineReaction,
                                "Decline Shield and take the hit".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::SecondWind => {
                        actions.insert(
                            "encounter-action:second-wind".to_owned(),
                            (EncounterIntent::SecondWind, "Use Second Wind".to_owned()),
                        );
                    }
                    LegalEncounterAction::ActionSurge => {
                        actions.insert(
                            "encounter-action:action-surge".to_owned(),
                            (EncounterIntent::ActionSurge, "Use Action Surge".to_owned()),
                        );
                    }
                    LegalEncounterAction::BeginShortRest => {
                        actions.insert(
                            "encounter-action:rest:short:begin".to_owned(),
                            (
                                EncounterIntent::BeginShortRest,
                                "Begin a short rest".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::SpendHitDie => {
                        actions.insert(
                            "encounter-action:rest:short:hit-die".to_owned(),
                            (
                                EncounterIntent::SpendHitDie,
                                "Confirm: spend one hit die".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::UseArcaneRecovery => {
                        actions.insert(
                            "encounter-action:rest:short:arcane-recovery".to_owned(),
                            (
                                EncounterIntent::UseArcaneRecovery,
                                "Use Arcane Recovery".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::FinishShortRest => {
                        actions.insert(
                            "encounter-action:rest:short:finish".to_owned(),
                            (
                                EncounterIntent::FinishShortRest,
                                "Finish the short rest".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::TakeLongRest => {
                        actions.insert(
                            "encounter-action:rest:long".to_owned(),
                            (
                                EncounterIntent::TakeLongRest,
                                "Take an eight-hour long rest".to_owned(),
                            ),
                        );
                    }
                    LegalEncounterAction::EndTurn => {
                        actions.insert(
                            "encounter-action:end-turn".to_owned(),
                            (EncounterIntent::EndTurn, "End the current turn".to_owned()),
                        );
                    }
                    LegalEncounterAction::RollDeathSave => {
                        actions.insert(
                            "encounter-action:death-save".to_owned(),
                            (
                                EncounterIntent::RollDeathSave,
                                "Roll the hero's death save".to_owned(),
                            ),
                        );
                    }
                }
            }
            let facts = actions
                .iter()
                .map(|(id, (_, label))| CommittedPublicFact {
                    fact_id: id.clone(),
                    summary: format!(
                        "Currently legal action: {label}. Use exactly action ID {id}."
                    ),
                })
                .collect();
            (actions, facts)
        }

        fn encounter_public_facts(
            state: &manchester_dnd_core::encounter::EncounterState,
        ) -> Vec<CommittedPublicFact> {
            let mut facts = vec![
                CommittedPublicFact {
                    fact_id: "state:encounter".to_owned(),
                    summary: format!(
                        "Encounter status {:?}, round {}, revision {}.",
                        state.status, state.round, state.revision
                    ),
                },
                CommittedPublicFact {
                    fact_id: "state:hero".to_owned(),
                    summary: format!(
                        "Hero {} has {}/{} HP at {} feet.",
                        state.hero.name,
                        state.hero.hit_points.current,
                        state.hero.hit_points.maximum,
                        state.hero.position_feet
                    ),
                },
                CommittedPublicFact {
                    fact_id: "state:creature".to_owned(),
                    summary: format!(
                        "Creature {} has {}/{} HP at {} feet.",
                        state.creature.name,
                        state.creature.hit_points.current,
                        state.creature.hit_points.maximum,
                        state.creature.position_feet
                    ),
                },
            ];
            if let Some(actor) = state.current_actor() {
                facts.push(CommittedPublicFact {
                    fact_id: "state:current-actor".to_owned(),
                    summary: format!("It is currently {}'s turn.", actor.name),
                });
            }
            facts
        }

        async fn prompt_policy(context: &ServerContext) -> GmPromptPolicy {
            let selected_pack_id = context
                .application
                .load_local_hero_workspace()
                .await
                .ok()
                .and_then(|workspace| workspace.character)
                .map(|hero| hero.choices.pins.theme_id.pack_id().to_owned());
            let pack = selected_pack_id
                .as_deref()
                .and_then(|id| context.active_content.pack(id))
                .unwrap_or_else(|| context.active_content.default_theme());
            let tokens = pack
                .theme_tokens()
                .expect("the active theme catalog guarantees theme tokens");
            let tone = if tokens.pack_id.contains("emberline") {
                "emberline"
            } else {
                "rainbound"
            };
            GmPromptPolicy {
                policy_id: "policy:private-mvp:v1".to_owned(),
                safety: GmSafetyPolicy {
                    audience: AudiencePolicy::AdultsOnly,
                    prohibited_categories: BTreeSet::from([
                        SafetyCategory::CurrentPersonalCrisis,
                        SafetyCategory::ImminentHarm,
                        SafetyCategory::PrivatePersonalData,
                        SafetyCategory::RealPersonLikeness,
                        SafetyCategory::SexualContentInvolvingMinors,
                    ]),
                    private_inspiration: PrivateInspirationPolicy::Excluded,
                    max_output_chars: 12_000,
                },
                theme: GmThemePolicy {
                    theme_id: tokens.theme_id.clone(),
                    tone_tags: vec![
                        "original-fantasy".to_owned(),
                        "mechanics-first".to_owned(),
                        tone.to_owned(),
                    ],
                    presentation_guidance: tokens.accessible_description.clone(),
                },
            }
        }

        fn narration_facts(committed: &CommittedEncounterOutcomeDto) -> Vec<MechanicalFact> {
            let mut facts = BTreeSet::new();
            for roll in &committed.resolution.rolls {
                facts.insert(MechanicalFact::Actor {
                    actor_id: roll.actor_id.clone(),
                });
                if let Some(target_id) = &roll.target_id {
                    facts.insert(MechanicalFact::Target {
                        target_id: target_id.clone(),
                    });
                }
                facts.insert(MechanicalFact::RollTotal {
                    roll_id: format!("roll:{}:{}", committed.event_sequence, roll.sequence),
                    total: roll.total,
                });
                if let (Some(target_id), Some(comparison)) = (&roll.target_id, &roll.comparison)
                    && comparison.kind == RollComparisonKind::ArmorClass
                {
                    facts.insert(MechanicalFact::ArmorClass {
                        target_id: target_id.clone(),
                        value: comparison.value,
                    });
                }
                let outcome = match (roll.purpose, roll.outcome) {
                    (EncounterRollPurpose::Attack, RawRollOutcome::Hit) => Some("attack:hit"),
                    (EncounterRollPurpose::Attack, RawRollOutcome::CriticalHit) => {
                        Some("attack:critical-hit")
                    }
                    (
                        EncounterRollPurpose::Attack,
                        RawRollOutcome::Miss | RawRollOutcome::AutomaticMiss,
                    ) => Some("attack:miss"),
                    (
                        EncounterRollPurpose::DeathSave,
                        RawRollOutcome::Success | RawRollOutcome::NaturalTwentyRecovery,
                    ) => Some("death-save:success"),
                    (
                        EncounterRollPurpose::DeathSave,
                        RawRollOutcome::Failure | RawRollOutcome::NaturalOneFailure,
                    ) => Some("death-save:failure"),
                    _ => None,
                };
                if let Some(outcome_id) = outcome {
                    facts.insert(MechanicalFact::Outcome {
                        outcome_id: outcome_id.to_owned(),
                    });
                }
            }
            for fact in &committed.resolution.facts {
                match fact {
                    EncounterFact::DamageApplied {
                        target_id,
                        amount,
                        current_hit_points_before,
                        current_hit_points_after,
                        ..
                    } => {
                        facts.insert(MechanicalFact::Damage {
                            target_id: target_id.clone(),
                            amount: *amount,
                        });
                        facts.insert(MechanicalFact::HitPoints {
                            target_id: target_id.clone(),
                            before: *current_hit_points_before,
                            after: *current_hit_points_after,
                        });
                    }
                    EncounterFact::HealingApplied {
                        actor_id,
                        current_hit_points_before,
                        current_hit_points_after,
                        ..
                    } => {
                        facts.insert(MechanicalFact::HitPoints {
                            target_id: actor_id.clone(),
                            before: *current_hit_points_before,
                            after: *current_hit_points_after,
                        });
                    }
                    EncounterFact::SpellCastResolved { spell, .. } => {
                        facts.insert(MechanicalFact::Outcome {
                            outcome_id: spell.mechanic_id().to_owned(),
                        });
                    }
                    EncounterFact::ClassFeatureResolved { feature, .. } => {
                        let outcome_id = match feature {
                            manchester_dnd_core::hero::FeatureId::SecondWind => {
                                "srd-5.1-cc:feature:second-wind"
                            }
                            manchester_dnd_core::hero::FeatureId::ActionSurge => {
                                "srd-5.1-cc:feature:action-surge"
                            }
                            _ => "manchester-arcana:feature:resolved",
                        };
                        facts.insert(MechanicalFact::Outcome {
                            outcome_id: outcome_id.to_owned(),
                        });
                    }
                    EncounterFact::LifeStatusChanged {
                        participant_id, to, ..
                    } => {
                        let condition_id = match to {
                            LifeStatus::Conscious => "condition:conscious",
                            LifeStatus::Unconscious => "condition:unconscious",
                            LifeStatus::Stable => "condition:stable",
                            LifeStatus::Dead => "condition:dead",
                        };
                        facts.insert(MechanicalFact::Condition {
                            target_id: participant_id.clone(),
                            condition_id: condition_id.to_owned(),
                        });
                    }
                    EncounterFact::ContextActionResolved { objective_id, .. } => {
                        facts.insert(MechanicalFact::Objective {
                            objective_id: objective_id.clone(),
                            status_id: "objective:completed".to_owned(),
                        });
                    }
                    EncounterFact::EncounterCompleted { outcome, .. } => {
                        facts.insert(MechanicalFact::Outcome {
                            outcome_id: format!("encounter:{outcome:?}").to_ascii_lowercase(),
                        });
                    }
                    _ => {}
                }
            }
            if facts.is_empty() {
                facts.insert(MechanicalFact::Outcome {
                    outcome_id: "encounter:command-committed".to_owned(),
                });
            }
            facts.into_iter().take(64).collect()
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(feature = "ssr")]
async fn replay_committed_typed_intent(
    context: &manchester_dnd_server::ServerContext,
    command: &TypedIntentCommand,
    receipt: manchester_dnd_server::repository::TypedIntentCommandReceipt,
    correlation_id: String,
) -> TypedIntentResponse {
    use manchester_dnd_core::{
        CommitEncounterCommand, ENCOUNTER_COMMIT_SCHEMA_VERSION, encounter::EncounterCommand,
    };

    let committed = match context
        .application
        .commit_encounter_command_with_correlation(
            CommitEncounterCommand {
                schema_version: ENCOUNTER_COMMIT_SCHEMA_VERSION,
                campaign_session_id: receipt.campaign_session_id.clone(),
                expected_campaign_revision: receipt.expected_campaign_revision,
                command: EncounterCommand::new(
                    receipt.expected_encounter_revision,
                    receipt.client_idempotency_key.clone(),
                    receipt.resolved_intent.clone(),
                ),
            },
            &correlation_id,
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            return TypedIntentResponse::Rejected(crate::campaign::public_error(
                &error,
                correlation_id,
            ));
        }
    };
    if context
        .generation_ledger
        .commit_typed_intent_command_receipt(
            &receipt,
            &command.player_intent,
            committed.event_sequence,
            committed.result_campaign_revision,
        )
        .await
        .is_err()
    {
        return TypedIntentResponse::Rejected(crate::campaign::internal_error(correlation_id));
    }
    let interpretation_evidence: GenerationEvidence =
        match serde_json::from_str(&receipt.interpretation_evidence_json) {
            Ok(evidence) => evidence,
            Err(_) => {
                return TypedIntentResponse::Rejected(crate::campaign::internal_error(
                    correlation_id,
                ));
            }
        };
    let engine_narration = committed.resolution.narration.authored_text.clone();
    let replay = narration_server::replay_recorded_narration(
        context,
        NarrationGenerationOrigin {
            campaign_session_id: &committed.campaign_session_id,
            campaign_revision: committed.result_campaign_revision,
            event_sequence: committed.event_sequence,
            idempotency_key: &command.idempotency_key,
            correlation_id: &correlation_id,
            engine_narration: &engine_narration,
            private_inspiration_work_id: None,
        },
    )
    .await;
    let (narration, narration_evidence, narration_versions) = match replay {
        RecordedNarrationReplay::Available(recorded) => {
            let recorded = *recorded;
            (
                recorded.narration,
                Some(recorded.evidence),
                recorded.versions,
            )
        }
        RecordedNarrationReplay::Expired { versions } => (engine_narration, None, versions),
        RecordedNarrationReplay::Missing => {
            let versions = context
                .generation_ledger
                .presentations_for_turn(&committed.campaign_session_id, committed.event_sequence)
                .await
                .map(|versions| {
                    versions
                        .into_iter()
                        .map(narration_server::presentation_view)
                        .collect()
                })
                .unwrap_or_default();
            (engine_narration, None, versions)
        }
    };
    TypedIntentResponse::Resolved(Box::new(TypedIntentResolution::Committed(Box::new(
        CommittedTypedIntent {
            outcome: committed,
            interpretation: format!(
                "Matched the description to the authored legal action: {}.",
                receipt.interpretation_label
            ),
            narration,
            interpretation_evidence,
            narration_evidence,
            narration_versions,
        },
    ))))
}

/// Generates only a new presentation for an already committed encounter turn.
/// This path never calls an application mutation, consumes RNG, or changes a
/// campaign/encounter revision.
#[server(input = Json)]
pub async fn regenerate_narration_presentation(
    command: RegenerateNarrationCommand,
) -> Result<RegenerateNarrationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_core::is_valid_opaque_id;
        use manchester_dnd_server::{ServerContext, repository::MAX_TEXT_PRESENTATION_VERSIONS};

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(RegenerateNarrationResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        if command.schema_version != NARRATION_REGENERATION_SCHEMA_VERSION
            || !is_valid_opaque_id(&command.campaign_session_id)
            || !is_valid_opaque_id(&command.idempotency_key)
            || command.event_sequence == 0
        {
            return Ok(RegenerateNarrationResponse::Rejected(public_retry_error(
                "invalid_narration_regeneration",
                "The narration retry request is malformed.",
                false,
                correlation_id,
            )));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(RegenerateNarrationResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let view = match context.application.load_local_campaign().await {
            Ok(view) => view,
            Err(error) => {
                return Ok(RegenerateNarrationResponse::Rejected(
                    crate::campaign::public_error(&error, correlation_id),
                ));
            }
        };
        if view.campaign_session_id != command.campaign_session_id {
            return Ok(RegenerateNarrationResponse::Rejected(public_retry_error(
                "wrong_campaign",
                "That committed turn does not belong to the current campaign.",
                false,
                correlation_id,
            )));
        }
        let committed = match context
            .generation_ledger
            .committed_encounter_outcome(&command.campaign_session_id, command.event_sequence)
            .await
        {
            Ok(outcome) => outcome,
            Err(_) => {
                return Ok(RegenerateNarrationResponse::Rejected(public_retry_error(
                    "narration_turn_unavailable",
                    "The immutable encounter turn for this narration is unavailable.",
                    false,
                    correlation_id,
                )));
            }
        };
        let engine_narration = committed.resolution.narration.authored_text.clone();
        let generation_origin = NarrationGenerationOrigin {
            campaign_session_id: &command.campaign_session_id,
            campaign_revision: committed.result_campaign_revision,
            event_sequence: command.event_sequence,
            idempotency_key: &command.idempotency_key,
            correlation_id: &correlation_id,
            engine_narration: &engine_narration,
            private_inspiration_work_id: None,
        };
        // Exact transport replay is checked before the three-version cap. A
        // lost response for version three therefore returns version three
        // instead of being mistaken for a fourth regeneration. The opaque
        // client key is resolved before current policy/config preparation, so
        // a deployment change cannot spend another presentation version.
        match narration_server::replay_recorded_narration(&context, generation_origin).await {
            RecordedNarrationReplay::Available(recorded) => {
                return Ok(RegenerateNarrationResponse::Regenerated(Box::new(
                    regenerated_response(command.event_sequence, *recorded),
                )));
            }
            RecordedNarrationReplay::Expired { .. } => {
                return Ok(RegenerateNarrationResponse::Rejected(public_retry_error(
                    "narration_replay_expired",
                    "That exact superseded narration body has reached its retention deadline. Its request key remains closed and cannot create another version.",
                    false,
                    correlation_id,
                )));
            }
            RecordedNarrationReplay::Missing => {}
        }
        let version_count = match context
            .generation_ledger
            .presentation_version_count(&command.campaign_session_id, command.event_sequence)
            .await
        {
            Ok(count) => count,
            Err(_) => {
                return Ok(RegenerateNarrationResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                ));
            }
        };
        if version_count >= MAX_TEXT_PRESENTATION_VERSIONS {
            return Ok(RegenerateNarrationResponse::Rejected(public_retry_error(
                "narration_regeneration_limit",
                "This turn already has its initial narration and two presentation-only retries.",
                false,
                correlation_id,
            )));
        }
        let Some(input) = narration_server::narration_input(&context, &committed).await else {
            return Ok(RegenerateNarrationResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let Some(recorded) =
            narration_server::generate_recorded_narration(&context, &input, generation_origin)
                .await
        else {
            return Ok(RegenerateNarrationResponse::Rejected(public_retry_error(
                "narration_regeneration_unavailable",
                "The presentation retry could not be retained. The committed mechanics and selected narration are unchanged.",
                true,
                correlation_id,
            )));
        };
        Ok(RegenerateNarrationResponse::Regenerated(Box::new(
            regenerated_response(command.event_sequence, recorded),
        )))
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(feature = "ssr")]
fn public_retry_error(
    code: &str,
    message: &str,
    retryable: bool,
    correlation_id: String,
) -> PublicGameError {
    PublicGameError {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable,
        current_revision: None,
        correlation_id,
        alternatives: Vec::new(),
    }
}

#[cfg(feature = "ssr")]
fn regenerated_response(event_sequence: u64, recorded: RecordedNarration) -> RegeneratedNarration {
    RegeneratedNarration {
        event_sequence,
        presentation_id: recorded.presentation_id,
        presentation_version: recorded.presentation_version,
        requested_presentation_selected: recorded.requested_presentation_selected,
        selected_presentation_version: recorded.selected_presentation_version,
        narration: recorded.narration,
        evidence: recorded.evidence,
        versions: recorded.versions,
    }
}

#[cfg(feature = "ssr")]
mod narration_server {
    use std::collections::BTreeSet;

    use manchester_dnd_core::{
        CommittedEncounterOutcomeDto,
        ai_turn::{MechanicalFact, ProposalAcceptanceContext, TypedGmProposal},
        encounter::{
            EncounterFact, EncounterRollPurpose, LifeStatus, RawRollOutcome, RollComparisonKind,
        },
    };
    use manchester_dnd_server::{
        InlineGenerationAttempt, InlineGenerationRequest, LlmBackend, ServerContext,
        inspiration::{
            CampaignInspirationTone, DerivedArtifactPolicy, DerivedWorkKind, InspirationAudience,
            InspirationMedia, OpaqueInspirationId, PRIVATE_INSPIRATION_SCHEMA_VERSION,
            RegisterDerivedWorkCommand, RequestInspirationSelectionCommand,
        },
        repository::{
            GeneratedTextPresentation, GeneratedTextPresentationReplay,
            GeneratedTextPresentationSource,
            jobs::{GenerationFailureCode, GenerationPurpose},
        },
        typed_gm::{
            AudiencePolicy, CommittedPublicFact, GenerationFailureClass, GmPromptPolicy,
            GmSafetyPolicy, GmThemePolicy, PrivateInspirationBrief, PrivateInspirationPolicy,
            SafetyCategory, TYPED_GM_PROMPT_TEMPLATE_ID, TypedGmPurpose, TypedGmTurnInput,
            TypedGmTurnResult, TypedProposalSource,
        },
    };

    use super::{
        GenerationEvidence, NarrationGenerationOrigin, NarrationPresentationView,
        RecordedNarration, RecordedNarrationReplay,
    };

    pub(super) struct ReservedNarrationInspiration {
        pub(super) brief: PrivateInspirationBrief,
        pub(super) work_id: String,
        pub(super) tone: CampaignSafetyTone,
    }

    #[derive(Clone, Copy)]
    pub(super) enum CampaignSafetyTone {
        Gothic,
        Hopeful,
        Lighthearted,
    }

    pub(super) fn generation_evidence(
        result: &TypedGmTurnResult,
        attempt: Option<&InlineGenerationAttempt>,
    ) -> GenerationEvidence {
        GenerationEvidence {
            job_id: attempt.map(|attempt| attempt.job_id.clone()),
            attempt_id: attempt.map(|attempt| attempt.attempt_id.clone()),
            source: match result.source {
                TypedProposalSource::Provider => "provider",
                TypedProposalSource::AuthoredFallback => "authored_fallback",
            }
            .to_owned(),
            failure: result
                .failure
                .map(|failure| failure_label(failure).to_owned()),
            attempts: result.attempts,
            prompt_fingerprint: result.prompt_fingerprint.as_str().to_owned(),
            policy_fingerprint: result.policy_fingerprint.as_str().to_owned(),
            config_fingerprint: result.config_fingerprint.as_str().to_owned(),
            proposal_fingerprint: result.proposal_fingerprint.as_str().to_owned(),
            model: result.model.clone(),
            prompt_tokens: result.usage.prompt_tokens,
            completion_tokens: result.usage.completion_tokens,
            total_tokens: result.usage.total_tokens,
        }
    }

    /// Reserves a source only for the deterministic fake provider approved by
    /// Q08. The repository derives the safe trigger window, checks every grant,
    /// persists the draw/cooldown, and then registers a cancellable text work
    /// item before any minimized fact reaches generation.
    pub(super) async fn reserve_narration_inspiration(
        context: &ServerContext,
        committed: &CommittedEncounterOutcomeDto,
    ) -> Option<ReservedNarrationInspiration> {
        if context.config.text_llm.backend != LlmBackend::Fake {
            return None;
        }
        let campaign_id = OpaqueInspirationId::new(committed.campaign_session_id.clone()).ok()?;
        let status = context
            .private_inspiration
            .campaign_status(&campaign_id)
            .await
            .ok()?;
        let settings = status.settings?;
        let tone = match settings.tone {
            CampaignInspirationTone::GothicAdventure => CampaignSafetyTone::Gothic,
            CampaignInspirationTone::HopefulAdventure => CampaignSafetyTone::Hopeful,
            CampaignInspirationTone::LightheartedAdventure => CampaignSafetyTone::Lighthearted,
        };
        let selection = context
            .private_inspiration
            .request_selection(RequestInspirationSelectionCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign_id.clone(),
                idempotency_key: OpaqueInspirationId::new(format!(
                    "inspiration-selection:{}",
                    committed.event_sequence
                ))
                .ok()?,
                expected_campaign_revision: committed.result_campaign_revision,
                expected_settings_revision: settings.revision,
                audience: InspirationAudience::PrivateCampaign,
                media: InspirationMedia::Text,
            })
            .await
            .ok()?;
        let prompt = selection.prompt?;
        let source_version = selection.outcome.source_version?;
        let source_id = prompt.privacy_source_id().to_owned();
        let selection_id = selection.outcome.selection_id;
        let work_id = OpaqueInspirationId::new(format!("derived-text:{selection_id}")).ok()?;
        let register_key =
            OpaqueInspirationId::new(format!("derived-register:{selection_id}")).ok()?;
        context
            .private_inspiration
            .register_derived_work(RegisterDerivedWorkCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id: campaign_id,
                idempotency_key: register_key,
                work_id: work_id.clone(),
                selection_id: selection_id.clone(),
                kind: DerivedWorkKind::Text,
                // The live narration path chooses the strongest policy even
                // when every participant allowed a weaker post-revocation
                // treatment.
                artifact_policy: DerivedArtifactPolicy::DeleteDerived,
            })
            .await
            .ok()?;
        let mut forbidden_identifiers = prompt
            .metadata
            .participant_aliases
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        forbidden_identifiers.insert(source_id.clone());
        Some(ReservedNarrationInspiration {
            brief: PrivateInspirationBrief {
                selection_id: selection_id.to_string(),
                source_id,
                source_version,
                source_digest: prompt.source_digest().clone(),
                minimized_facts: prompt.inspiration().facts().to_vec(),
                forbidden_identifiers,
                transformation: prompt.transformation_policy(),
            },
            work_id: work_id.to_string(),
            tone,
        })
    }

    pub(super) async fn narration_input(
        context: &ServerContext,
        committed: &CommittedEncounterOutcomeDto,
    ) -> Option<TypedGmTurnInput> {
        let policy = prompt_policy(context).await;
        let fingerprints = context.typed_game_master.fingerprints(&policy).ok()?;
        Some(TypedGmTurnInput {
            purpose: TypedGmPurpose::NarrateCommittedFacts,
            acceptance: ProposalAcceptanceContext {
                session_id: committed.campaign_session_id.clone(),
                revision: committed.result_campaign_revision,
                event_sequence: committed.event_sequence,
                prompt_template_id: TYPED_GM_PROMPT_TEMPLATE_ID.to_owned(),
                policy_id: policy.policy_id.clone(),
                config_fingerprint: fingerprints.config,
                legal_action_ids: BTreeSet::new(),
                legal_check_ids: BTreeSet::new(),
                legal_target_ids: BTreeSet::new(),
                legal_scene_ids: BTreeSet::new(),
                legal_objective_ids: BTreeSet::new(),
                authoritative_facts: narration_facts(committed),
            },
            public_facts: vec![CommittedPublicFact {
                fact_id: "fact:committed-engine-narration".to_owned(),
                summary: committed.resolution.narration.authored_text.clone(),
            }],
            player_intent: None,
            private_inspiration: None,
            policy,
        })
    }

    pub(super) async fn generate_recorded_narration(
        context: &ServerContext,
        input: &TypedGmTurnInput,
        origin: NarrationGenerationOrigin<'_>,
    ) -> Option<RecordedNarration> {
        let NarrationGenerationOrigin {
            campaign_session_id,
            campaign_revision,
            event_sequence,
            idempotency_key,
            correlation_id,
            engine_narration,
            private_inspiration_work_id,
        } = origin;
        match replay_recorded_narration(context, origin).await {
            RecordedNarrationReplay::Available(replayed) => return Some(*replayed),
            RecordedNarrationReplay::Expired { .. } => {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
                return None;
            }
            RecordedNarrationReplay::Missing => {}
        }
        let origin_turn_id = match context
            .generation_ledger
            .origin_turn_id(campaign_session_id, event_sequence)
            .await
        {
            Ok(origin_turn_id) => origin_turn_id,
            Err(_) => {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
                return None;
            }
        };
        let prepared = match context.typed_game_master.prepare_request(input) {
            Ok(prepared) => prepared,
            Err(_) => {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
                return None;
            }
        };
        let generation_key = generation_key(
            event_sequence,
            &prepared.fingerprints.policy,
            idempotency_key,
        );
        let attempt = match context
            .generation_ledger
            .begin(InlineGenerationRequest {
                campaign_session_id: campaign_session_id.to_owned(),
                origin_turn_id: Some(origin_turn_id),
                origin_campaign_revision: campaign_revision,
                purpose: GenerationPurpose::Narration,
                // Q10 binds every regeneration to both the immutable turn and
                // the exact policy while the caller key keeps retries distinct.
                idempotency_key: generation_key,
                input_digest: prepared.request_fingerprint,
                prompt_digest: prepared.fingerprints.prompt,
                policy_digest: prepared.fingerprints.policy,
                config_digest: prepared.fingerprints.config,
                correlation_id: correlation_id.to_owned(),
            })
            .await
        {
            Ok(attempt) => attempt,
            Err(_) => {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
                return None;
            }
        };
        if context.config.text_llm.backend == LlmBackend::Fake {
            let result = match context.typed_game_master.generate(input).await {
                Ok(result) => result,
                Err(_) => {
                    abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                        .await;
                    if context
                        .generation_ledger
                        .finish_engine_authored_presentation(
                            &attempt,
                            engine_narration,
                            idempotency_key,
                            GenerationFailureCode::ProviderUnavailable,
                        )
                        .await
                        .is_err()
                    {
                        let _ = context.generation_ledger.finish_unavailable(&attempt).await;
                        return None;
                    }
                    return match replay_recorded_narration(context, origin).await {
                        RecordedNarrationReplay::Available(recorded) => Some(*recorded),
                        RecordedNarrationReplay::Missing
                        | RecordedNarrationReplay::Expired { .. } => None,
                    };
                }
            };
            let TypedGmProposal::Narration(proposal) = &result.proposal else {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
                let _ = context.generation_ledger.finish_unavailable(&attempt).await;
                return None;
            };
            let text = proposal.text.clone();
            let bind_private_work = matches!(result.source, TypedProposalSource::Provider)
                .then_some(private_inspiration_work_id)
                .flatten();
            if bind_private_work.is_none() {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
            }
            if context
                .generation_ledger
                .finish_typed_presentation(
                    &attempt,
                    &result,
                    &text,
                    idempotency_key,
                    bind_private_work,
                )
                .await
                .is_err()
            {
                abandon_private_work(context, campaign_session_id, private_inspiration_work_id)
                    .await;
                if context
                    .generation_ledger
                    .finish_engine_authored_presentation(
                        &attempt,
                        engine_narration,
                        idempotency_key,
                        GenerationFailureCode::UnsafeOutput,
                    )
                    .await
                    .is_err()
                {
                    let _ = context.generation_ledger.finish_unavailable(&attempt).await;
                    return None;
                }
            }
            let _ = generation_evidence(&result, Some(&attempt));
        } else {
            abandon_private_work(context, campaign_session_id, private_inspiration_work_id).await;
            // External free-form prose remains fail-closed until an
            // independently verified moderation and fact-entailment gate is
            // deployed. The engine-authored body is still a durable version.
            let failure = match context.config.text_llm.backend {
                LlmBackend::Disabled => GenerationFailureCode::ProviderUnavailable,
                LlmBackend::OpenAiCompatible => GenerationFailureCode::UnsafeOutput,
                LlmBackend::Fake => unreachable!("the fake branch returned above"),
            };
            let stored = context
                .generation_ledger
                .finish_engine_authored_presentation(
                    &attempt,
                    engine_narration,
                    idempotency_key,
                    failure,
                )
                .await
                .ok()?;
            let _evidence = GenerationEvidence {
                job_id: Some(attempt.job_id.clone()),
                attempt_id: Some(attempt.attempt_id.clone()),
                source: "engine_authored".to_owned(),
                failure: Some(
                    match failure {
                        GenerationFailureCode::ProviderUnavailable => "unavailable",
                        GenerationFailureCode::UnsafeOutput => "external_prose_blocked",
                        _ => "generation_failed",
                    }
                    .to_owned(),
                ),
                attempts: 0,
                prompt_fingerprint: stored.prompt_digest.as_str().to_owned(),
                policy_fingerprint: stored.policy_digest.as_str().to_owned(),
                config_fingerprint: stored.config_digest.as_str().to_owned(),
                proposal_fingerprint: stored.output_digest.as_str().to_owned(),
                model: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
            };
        }
        match replay_recorded_narration(context, origin).await {
            RecordedNarrationReplay::Available(recorded) => Some(*recorded),
            RecordedNarrationReplay::Missing | RecordedNarrationReplay::Expired { .. } => None,
        }
    }

    async fn abandon_private_work(
        context: &ServerContext,
        campaign_session_id: &str,
        work_id: Option<&str>,
    ) {
        let (Ok(campaign_session_id), Some(Ok(work_id))) = (
            OpaqueInspirationId::new(campaign_session_id.to_owned()),
            work_id.map(|work_id| OpaqueInspirationId::new(work_id.to_owned())),
        ) else {
            return;
        };
        let _ = context
            .private_inspiration
            .abandon_pending_derived_work(&campaign_session_id, &work_id)
            .await;
    }

    pub(super) async fn replay_recorded_narration(
        context: &ServerContext,
        origin: NarrationGenerationOrigin<'_>,
    ) -> RecordedNarrationReplay {
        let replay = match context
            .generation_ledger
            .presentation_replay_for_client_key(
                origin.campaign_session_id,
                origin.event_sequence,
                origin.idempotency_key,
            )
            .await
        {
            Ok(Some(replay)) => replay,
            Ok(None) | Err(_) => return RecordedNarrationReplay::Missing,
        };
        match replay {
            GeneratedTextPresentationReplay::Available(snapshot) => {
                let requested = presentation_view(snapshot.requested);
                let versions = snapshot
                    .retained_versions
                    .into_iter()
                    .map(presentation_view)
                    .collect::<Vec<_>>();
                let selected_presentation_version = versions
                    .iter()
                    .find(|version| version.selected)
                    .map(|version| version.version);
                RecordedNarrationReplay::Available(Box::new(RecordedNarration {
                    presentation_id: requested.presentation_id.clone(),
                    presentation_version: requested.version,
                    requested_presentation_selected: requested.selected,
                    selected_presentation_version,
                    narration: requested.body.clone(),
                    evidence: presentation_evidence(&requested),
                    versions,
                }))
            }
            GeneratedTextPresentationReplay::Expired {
                retained_versions, ..
            } => RecordedNarrationReplay::Expired {
                versions: retained_versions
                    .into_iter()
                    .map(presentation_view)
                    .collect(),
            },
        }
    }

    fn generation_key(
        event_sequence: u64,
        policy_digest: &manchester_dnd_core::Sha256Digest,
        idempotency_key: &str,
    ) -> String {
        let policy_key = &policy_digest.as_str()["sha256:".len()..23];
        format!("narration:{event_sequence}:{policy_key}:{idempotency_key}")
    }

    fn presentation_evidence(presentation: &NarrationPresentationView) -> GenerationEvidence {
        GenerationEvidence {
            job_id: Some(presentation.generation_job_id.clone()),
            attempt_id: Some(presentation.generation_attempt_id.clone()),
            source: presentation.source.clone(),
            failure: match presentation.source.as_str() {
                "provider" => None,
                "authored_fallback" => Some("authored_fallback".to_owned()),
                "engine_authored" => Some("external_or_provider_prose_unavailable".to_owned()),
                _ => Some("generation_failed".to_owned()),
            },
            attempts: 0,
            prompt_fingerprint: presentation.prompt_digest.clone(),
            policy_fingerprint: presentation.policy_digest.clone(),
            config_fingerprint: presentation.config_digest.clone(),
            proposal_fingerprint: presentation.output_digest.clone(),
            model: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
        }
    }

    pub(super) async fn prompt_policy(context: &ServerContext) -> GmPromptPolicy {
        let selected_pack_id = context
            .application
            .load_local_hero_workspace()
            .await
            .ok()
            .and_then(|workspace| workspace.character)
            .map(|hero| hero.choices.pins.theme_id.pack_id().to_owned());
        let pack = selected_pack_id
            .as_deref()
            .and_then(|id| context.active_content.pack(id))
            .unwrap_or_else(|| context.active_content.default_theme());
        let tokens = pack
            .theme_tokens()
            .expect("the active theme catalog guarantees theme tokens");
        let tone = if tokens.pack_id.contains("emberline") {
            "emberline"
        } else {
            "rainbound"
        };
        GmPromptPolicy {
            policy_id: "policy:private-mvp:v1".to_owned(),
            safety: GmSafetyPolicy {
                audience: AudiencePolicy::AdultsOnly,
                prohibited_categories: BTreeSet::from([
                    SafetyCategory::CurrentPersonalCrisis,
                    SafetyCategory::ImminentHarm,
                    SafetyCategory::PrivatePersonalData,
                    SafetyCategory::RealPersonLikeness,
                    SafetyCategory::SexualContentInvolvingMinors,
                ]),
                private_inspiration: PrivateInspirationPolicy::Excluded,
                max_output_chars: 12_000,
            },
            theme: GmThemePolicy {
                theme_id: tokens.theme_id.clone(),
                tone_tags: vec![
                    "original-fantasy".to_owned(),
                    "mechanics-first".to_owned(),
                    tone.to_owned(),
                ],
                presentation_guidance: tokens.accessible_description.clone(),
            },
        }
    }

    pub(super) fn narration_facts(committed: &CommittedEncounterOutcomeDto) -> Vec<MechanicalFact> {
        let mut facts = BTreeSet::new();
        for roll in &committed.resolution.rolls {
            facts.insert(MechanicalFact::Actor {
                actor_id: roll.actor_id.clone(),
            });
            if let Some(target_id) = &roll.target_id {
                facts.insert(MechanicalFact::Target {
                    target_id: target_id.clone(),
                });
            }
            facts.insert(MechanicalFact::RollTotal {
                roll_id: format!("roll:{}:{}", committed.event_sequence, roll.sequence),
                total: roll.total,
            });
            if let (Some(target_id), Some(comparison)) = (&roll.target_id, &roll.comparison)
                && comparison.kind == RollComparisonKind::ArmorClass
            {
                facts.insert(MechanicalFact::ArmorClass {
                    target_id: target_id.clone(),
                    value: comparison.value,
                });
            }
            let outcome = match (roll.purpose, roll.outcome) {
                (EncounterRollPurpose::Attack, RawRollOutcome::Hit) => Some("attack:hit"),
                (EncounterRollPurpose::Attack, RawRollOutcome::CriticalHit) => {
                    Some("attack:critical-hit")
                }
                (
                    EncounterRollPurpose::Attack,
                    RawRollOutcome::Miss | RawRollOutcome::AutomaticMiss,
                ) => Some("attack:miss"),
                (
                    EncounterRollPurpose::DeathSave,
                    RawRollOutcome::Success | RawRollOutcome::NaturalTwentyRecovery,
                ) => Some("death-save:success"),
                (
                    EncounterRollPurpose::DeathSave,
                    RawRollOutcome::Failure | RawRollOutcome::NaturalOneFailure,
                ) => Some("death-save:failure"),
                _ => None,
            };
            if let Some(outcome_id) = outcome {
                facts.insert(MechanicalFact::Outcome {
                    outcome_id: outcome_id.to_owned(),
                });
            }
        }
        for fact in &committed.resolution.facts {
            match fact {
                EncounterFact::DamageApplied {
                    target_id,
                    amount,
                    current_hit_points_before,
                    current_hit_points_after,
                    ..
                } => {
                    facts.insert(MechanicalFact::Damage {
                        target_id: target_id.clone(),
                        amount: *amount,
                    });
                    facts.insert(MechanicalFact::HitPoints {
                        target_id: target_id.clone(),
                        before: *current_hit_points_before,
                        after: *current_hit_points_after,
                    });
                }
                EncounterFact::HealingApplied {
                    actor_id,
                    current_hit_points_before,
                    current_hit_points_after,
                    ..
                } => {
                    facts.insert(MechanicalFact::HitPoints {
                        target_id: actor_id.clone(),
                        before: *current_hit_points_before,
                        after: *current_hit_points_after,
                    });
                }
                EncounterFact::SpellCastResolved { spell, .. } => {
                    facts.insert(MechanicalFact::Outcome {
                        outcome_id: spell.mechanic_id().to_owned(),
                    });
                }
                EncounterFact::ClassFeatureResolved { feature, .. } => {
                    let outcome_id = match feature {
                        manchester_dnd_core::hero::FeatureId::SecondWind => {
                            "srd-5.1-cc:feature:second-wind"
                        }
                        manchester_dnd_core::hero::FeatureId::ActionSurge => {
                            "srd-5.1-cc:feature:action-surge"
                        }
                        _ => "manchester-arcana:feature:resolved",
                    };
                    facts.insert(MechanicalFact::Outcome {
                        outcome_id: outcome_id.to_owned(),
                    });
                }
                EncounterFact::LifeStatusChanged {
                    participant_id, to, ..
                } => {
                    let condition_id = match to {
                        LifeStatus::Conscious => "condition:conscious",
                        LifeStatus::Unconscious => "condition:unconscious",
                        LifeStatus::Stable => "condition:stable",
                        LifeStatus::Dead => "condition:dead",
                    };
                    facts.insert(MechanicalFact::Condition {
                        target_id: participant_id.clone(),
                        condition_id: condition_id.to_owned(),
                    });
                }
                EncounterFact::ContextActionResolved { objective_id, .. } => {
                    facts.insert(MechanicalFact::Objective {
                        objective_id: objective_id.clone(),
                        status_id: "objective:completed".to_owned(),
                    });
                }
                EncounterFact::EncounterCompleted { outcome, .. } => {
                    facts.insert(MechanicalFact::Outcome {
                        outcome_id: format!("encounter:{outcome:?}").to_ascii_lowercase(),
                    });
                }
                _ => {}
            }
        }
        if facts.is_empty() {
            facts.insert(MechanicalFact::Outcome {
                outcome_id: "encounter:command-committed".to_owned(),
            });
        }
        facts.into_iter().take(64).collect()
    }

    pub(super) fn presentation_view(
        presentation: GeneratedTextPresentation,
    ) -> NarrationPresentationView {
        let private_inspiration_used = presentation.private_inspiration_work_id.is_some();
        NarrationPresentationView {
            presentation_id: presentation.id,
            version: presentation.version,
            selected: presentation.selected,
            source: match presentation.source {
                GeneratedTextPresentationSource::Provider => "provider",
                GeneratedTextPresentationSource::AuthoredFallback => "authored_fallback",
                GeneratedTextPresentationSource::EngineAuthored => "engine_authored",
            }
            .to_owned(),
            private_inspiration_used,
            privacy_redacted: presentation.privacy_redacted,
            body: presentation.body,
            generation_job_id: presentation.generation_job_id,
            generation_attempt_id: presentation.generation_attempt_id,
            config_digest: presentation.config_digest.as_str().to_owned(),
            prompt_digest: presentation.prompt_digest.as_str().to_owned(),
            policy_digest: presentation.policy_digest.as_str().to_owned(),
            output_digest: presentation.output_digest.as_str().to_owned(),
            retention_delete_after: presentation.retention_delete_after,
            created_at: presentation.created_at,
        }
    }

    const fn failure_label(failure: GenerationFailureClass) -> &'static str {
        match failure {
            GenerationFailureClass::Timeout => "timeout",
            GenerationFailureClass::Unavailable => "unavailable",
            GenerationFailureClass::RateLimit => "rate_limited",
            GenerationFailureClass::Malformed => "malformed",
            GenerationFailureClass::Unsafe => "unsafe",
            GenerationFailureClass::Contradiction => "contradiction",
        }
    }
}

#[component]
pub fn FreeformIntent(
    state: FreeformIntentState,
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    encounter_pending: RwSignal<bool>,
    encounter_notice: RwSignal<String>,
) -> impl IntoView {
    let FreeformIntentState {
        intent,
        result,
        intent_retry_command,
        retry_command,
    } = state;

    let submit = move |_| {
        let Some(view) = campaign_view.get_untracked() else {
            encounter_notice.set("Reload the campaign before describing an action.".to_owned());
            return;
        };
        let Some(encounter) = view.encounter.as_ref() else {
            encounter_notice
                .set("Resolve the runes before describing an encounter action.".to_owned());
            return;
        };
        let recovering_interrupted_request = intent_retry_command.get_untracked().is_some();
        let command = if let Some(command) = intent_retry_command.get_untracked() {
            command
        } else {
            let player_intent = intent.get_untracked().trim().to_owned();
            if player_intent.is_empty() {
                encounter_notice
                    .set("Describe one action before asking the game master.".to_owned());
                return;
            }
            TypedIntentCommand {
                schema_version: TYPED_INTENT_COMMAND_SCHEMA_VERSION,
                campaign_session_id: view.campaign_session_id.clone(),
                expected_campaign_revision: view.revision,
                expected_encounter_revision: encounter.state.revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                player_intent,
            }
        };
        intent_retry_command.set(Some(command.clone()));
        encounter_pending.set(true);
        encounter_notice.set(if recovering_interrupted_request {
            "Submitting the exact retained action request; a committed receipt will replay without model work or dice…".to_owned()
        } else {
            "Interpreting only against the current legal action IDs…".to_owned()
        });
        spawn_local(async move {
            match submit_typed_player_intent(command).await {
                Ok(TypedIntentResponse::Resolved(resolution)) => {
                    intent_retry_command.set(None);
                    let resolution = *resolution;
                    if let TypedIntentResolution::Committed(committed) = &resolution {
                        let outcome = &committed.outcome;
                        let narration = &committed.narration;
                        campaign_view.update(|current| {
                            if let Some(view) = current {
                                view.revision = outcome.result_campaign_revision;
                                view.last_event_sequence = outcome.event_sequence;
                                if let Some(encounter) = &mut view.encounter {
                                    encounter.campaign_revision = outcome.result_campaign_revision;
                                    encounter.last_event_sequence = outcome.event_sequence;
                                    encounter.state = outcome.resolution.state.clone();
                                    encounter.legal_actions.clone_from(&outcome.legal_actions);
                                    encounter.latest_outcome = Some(outcome.clone());
                                }
                            }
                        });
                        intent.set(String::new());
                        retry_command.set(None);
                        encounter_notice.set(format!("Mechanics saved first. {narration}"));
                    } else {
                        encounter_notice.set("No mechanic changed. Review the constrained response below.".to_owned());
                    }
                    result.set(Some(resolution));
                }
                Ok(TypedIntentResponse::Rejected(error)) => {
                    if !retain_typed_intent_for_recovery(&error) {
                        intent_retry_command.set(None);
                    }
                    encounter_notice.set(format!(
                        "{} [{}; reference {}]",
                        error.message, error.code, error.correlation_id
                    ));
                    // The existing encounter reload control performs explicit
                    // reconciliation; never silently resubmit a different intent.
                }
                Err(_) => encounter_notice.set(
                    "The response was interrupted. Click recover to submit the exact retained action key; committed mechanics, provider work, and dice will not run again."
                        .to_owned(),
                ),
            }
            encounter_pending.set(false);
        });
    };

    let retry_narration = move |_| {
        let Some(TypedIntentResolution::Committed(committed)) = result.get_untracked() else {
            encounter_notice.set("Commit a mechanic before retrying its presentation.".to_owned());
            return;
        };
        if committed.narration_versions.len() >= 3 {
            encounter_notice.set(
                "This turn has used both presentation-only retries. Its mechanics remain unchanged."
                    .to_owned(),
            );
            return;
        }
        let command = retry_command
            .get_untracked()
            .unwrap_or_else(|| RegenerateNarrationCommand {
                schema_version: NARRATION_REGENERATION_SCHEMA_VERSION,
                campaign_session_id: committed.outcome.campaign_session_id.clone(),
                event_sequence: committed.outcome.event_sequence,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            });
        // Retain this exact command until a structured server response arrives.
        // A transport interruption must not mint another generation key/version.
        retry_command.set(Some(command.clone()));
        encounter_pending.set(true);
        encounter_notice.set(
            "Retrying narration presentation only; saved mechanics and dice will not run again."
                .to_owned(),
        );
        spawn_local(async move {
            match regenerate_narration_presentation(command).await {
                Ok(RegenerateNarrationResponse::Regenerated(regenerated)) => {
                    retry_command.set(None);
                    let regenerated = *regenerated;
                    let requested_version = regenerated.presentation_version;
                    let requested_selected = regenerated.requested_presentation_selected;
                    let selected_version = regenerated.selected_presentation_version;
                    result.update(|current| {
                        if let Some(TypedIntentResolution::Committed(committed)) = current {
                            committed.narration = regenerated.narration;
                            committed.narration_evidence = Some(regenerated.evidence);
                            committed.narration_versions = regenerated.versions;
                        }
                    });
                    let selection = if requested_selected {
                        format!("Narration version {requested_version} selected.")
                    } else {
                        format!(
                            "Recovered exact narration version {requested_version}; version {} is currently selected.",
                            selected_version.map_or_else(|| "none".to_owned(), |value| value.to_string())
                        )
                    };
                    encounter_notice.set(format!(
                        "{selection} No mechanic, roll, HP, XP, or revision changed."
                    ));
                }
                Ok(RegenerateNarrationResponse::Rejected(error)) => {
                    retry_command.set(None);
                    encounter_notice.set(format!(
                        "{} [{}; reference {}]",
                        error.message, error.code, error.correlation_id
                    ));
                }
                Err(_) => encounter_notice.set(
                    "The presentation response was interrupted. Click retry again to replay the exact same request; the selected narration and all committed mechanics remain unchanged."
                        .to_owned(),
                ),
            }
            encounter_pending.set(false);
        });
    };

    view! {
        <div class="freeform-intent">
            <label for="freeform-intent">"Describe another action"</label>
            <textarea
                id="freeform-intent"
                rows="3"
                maxlength="4000"
                placeholder="For example: move toward the sluice, then release it."
                prop:value=move || intent.get()
                on:input=move |event| intent.set(event_target_value(&event))
                disabled=move || encounter_pending.get()
                    || campaign_loading.get()
                    || intent_retry_command.get().is_some()
                    || campaign_view.get().and_then(|view| view.encounter).is_none_or(|encounter| encounter.legal_actions.is_empty())
            ></textarea>
            <button
                class="refresh-button"
                disabled=move || encounter_pending.get()
                    || campaign_loading.get()
                    || (intent_retry_command.get().is_none()
                        && (intent.get().trim().is_empty()
                            || campaign_view.get().and_then(|view| view.encounter).is_none_or(|encounter| encounter.legal_actions.is_empty())))
                on:click=submit
            >
                {move || if intent_retry_command.get().is_some() {
                    "Recover interrupted action"
                } else {
                    "Interpret against legal actions"
                }}
            </button>
            <p>"The model can select only an action ID the Rust engine already offers. It cannot set dice, AC, DC, HP, damage, XP, actor, target legality, or revisions."</p>

            {move || result.get().map(|resolution| match resolution {
                TypedIntentResolution::Committed(committed) => {
                    let committed = *committed;
                    let retry_limit_reached = committed.narration_versions.len() >= 3;
                    view! {
                        <div class="typed-gm-result" role="status">
                            <strong>"Saved interpretation"</strong>
                            <p>{committed.interpretation}</p>
                            <p>{committed.narration}</p>
                            <button
                                class="refresh-button"
                                disabled=move || encounter_pending.get() || retry_limit_reached
                                on:click=retry_narration
                            >
                                {move || if retry_limit_reached {
                                    "Presentation retry limit reached"
                                } else if retry_command.get().is_some() {
                                    "Recover interrupted narration retry"
                                } else {
                                    "Retry narration presentation only"
                                }}
                            </button>
                            <p>"A retry creates another narration version for this saved turn. It cannot re-commit the command, consume RNG, change damage, or award XP."</p>
                            <NarrationHistory versions=committed.narration_versions/>
                            <GenerationDetails label="Interpretation evidence" evidence=committed.interpretation_evidence/>
                            {committed.narration_evidence.map(|evidence| view! { <GenerationDetails label="Narration evidence" evidence/> })}
                        </div>
                    }.into_any()
                },
                TypedIntentResolution::Clarification { question, choices, evidence } => view! {
                    <div class="typed-gm-result" role="status">
                        <strong>"Clarification required"</strong>
                        <p>{question}</p>
                        <ul>{choices.into_iter().map(|choice| view! { <li>{choice.label}</li> }).collect_view()}</ul>
                        <GenerationDetails label="Clarification evidence" evidence/>
                    </div>
                }.into_any(),
                TypedIntentResolution::Degraded { message, authored_alternatives, evidence } => view! {
                    <div class="typed-gm-result degraded" role="status">
                        <strong>"Deterministic degraded mode"</strong>
                        <p>{message}</p>
                        <p>"Available authored actions: " {authored_alternatives.join("; ")}</p>
                        <GenerationDetails label="Fallback evidence" evidence/>
                    </div>
                }.into_any(),
            })}
        </div>
    }
}

fn retain_typed_intent_for_recovery(error: &PublicGameError) -> bool {
    if matches!(
        error.code.as_str(),
        "revision_conflict"
            | "encounter_revision_conflict"
            | "lifecycle_revision_conflict"
            | "idempotency_conflict"
            | "invalid_typed_intent"
            | "unsupported_mechanic"
    ) {
        return false;
    }
    error.code == "internal_error" || error.retryable
}

#[component]
fn NarrationHistory(versions: Vec<NarrationPresentationView>) -> impl IntoView {
    if versions.is_empty() {
        return view! {
            <p class="degraded">"Narration provenance could not be retained; the engine-authored text remains playable and the mechanics are already saved."</p>
        }
        .into_any();
    }
    view! {
        <details class="narration-history">
            <summary>"Owner-visible narration versions (" {versions.len()} "/3)"</summary>
            <ol>
                {versions.into_iter().map(|version| {
                    let selection = if version.selected { "selected" } else { "unselected" };
                    let retention = if version.retention_delete_after.is_some() {
                        "Superseded body · 30-day retention policy"
                    } else {
                        "Selected body · campaign-lifetime retention policy"
                    };
                    let private_inspiration = version.private_inspiration_used.then_some({
                        if version.privacy_redacted {
                            "Removed at a participant request"
                        } else {
                            "Consented, minimized, high-fiction-distance source used"
                        }
                    });
                    view! {
                        <li>
                            <strong>"Version " {version.version} " — " {selection}</strong>
                            <p>{version.body}</p>
                            <dl class="generation-evidence">
                                <div><dt>"Source"</dt><dd>{version.source}</dd></div>
                                {private_inspiration.map(|status| view! {
                                    <div><dt>"Private inspiration"</dt><dd>{status}</dd></div>
                                })}
                                <div><dt>"Retention"</dt><dd>{retention}</dd></div>
                                <div><dt>"Durable job"</dt><dd class="digest">{version.generation_job_id}</dd></div>
                                <div><dt>"Durable attempt"</dt><dd class="digest">{version.generation_attempt_id}</dd></div>
                                <div><dt>"Prompt fingerprint"</dt><dd class="digest">{version.prompt_digest}</dd></div>
                                <div><dt>"Policy fingerprint"</dt><dd class="digest">{version.policy_digest}</dd></div>
                                <div><dt>"Config fingerprint"</dt><dd class="digest">{version.config_digest}</dd></div>
                                <div><dt>"Output fingerprint"</dt><dd class="digest">{version.output_digest}</dd></div>
                                <div><dt>"Presentation ID"</dt><dd class="digest">{version.presentation_id}</dd></div>
                            </dl>
                        </li>
                    }
                }).collect_view()}
            </ol>
        </details>
    }
    .into_any()
}

#[component]
fn GenerationDetails(label: &'static str, evidence: GenerationEvidence) -> impl IntoView {
    view! {
        <details class="generation-evidence">
            <summary>{label}</summary>
            <dl>
                <div><dt>"Source"</dt><dd>{evidence.source}</dd></div>
                <div><dt>"Failure"</dt><dd>{evidence.failure.unwrap_or_else(|| "none".to_owned())}</dd></div>
                <div><dt>"Attempts"</dt><dd>{evidence.attempts}</dd></div>
                {evidence.job_id.map(|job_id| view! { <div><dt>"Durable job"</dt><dd class="digest">{job_id}</dd></div> })}
                {evidence.attempt_id.map(|attempt_id| view! { <div><dt>"Durable attempt"</dt><dd class="digest">{attempt_id}</dd></div> })}
                <div><dt>"Model"</dt><dd>{evidence.model.unwrap_or_else(|| "disabled/fallback".to_owned())}</dd></div>
                <div><dt>"Tokens"</dt><dd>{evidence.total_tokens.map_or_else(|| "unreported".to_owned(), |tokens| tokens.to_string())}</dd></div>
                <div><dt>"Prompt fingerprint"</dt><dd class="digest">{evidence.prompt_fingerprint}</dd></div>
                <div><dt>"Policy fingerprint"</dt><dd class="digest">{evidence.policy_fingerprint}</dd></div>
                <div><dt>"Config fingerprint"</dt><dd class="digest">{evidence.config_fingerprint}</dd></div>
                <div><dt>"Proposal fingerprint"</dt><dd class="digest">{evidence.proposal_fingerprint}</dd></div>
            </dl>
        </details>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_error(code: &str, retryable: bool) -> PublicGameError {
        PublicGameError {
            code: code.to_owned(),
            message: "safe".to_owned(),
            retryable,
            current_revision: None,
            correlation_id: "correlation:test".to_owned(),
            alternatives: Vec::new(),
        }
    }

    #[test]
    fn typed_command_recovery_retains_only_safe_transient_rejections() {
        assert!(retain_typed_intent_for_recovery(&public_error(
            "internal_error",
            true,
        )));
        assert!(retain_typed_intent_for_recovery(&public_error(
            "temporarily_unavailable",
            true,
        )));
        assert!(!retain_typed_intent_for_recovery(&public_error(
            "revision_conflict",
            true,
        )));
        assert!(!retain_typed_intent_for_recovery(&public_error(
            "idempotency_conflict",
            false,
        )));
        assert!(!retain_typed_intent_for_recovery(&public_error(
            "unsupported_mechanic",
            false,
        )));
    }
}
