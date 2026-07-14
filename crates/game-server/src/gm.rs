use std::{collections::BTreeSet, sync::Arc};

use manchester_dnd_core::{
    AI_PROPOSAL_SCHEMA_VERSION, AiGmProposal, Character, ProposedEffect, SessionDto,
    SessionEventDto, SessionStatus, Sha256Digest, is_valid_opaque_id,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::GameMasterError,
    generation::{
        ChatMessage, TextGenerationRequest, TextGenerator, TextResponseFormat, TokenUsage,
    },
};

const REQUEST_SCHEMA_VERSION: u16 = 1;
const MAX_EFFECTS: usize = 12;
const MAX_NARRATIVE_CHARS: usize = 12_000;
const MAX_IMAGE_PROMPT_CHARS: usize = 4_000;
const MAX_REASON_CHARS: usize = 1_000;
const SYSTEM_PROMPT: &str = include_str!("../../../prompts/system/game-master.md");

#[derive(Debug, Clone, Serialize)]
pub struct EventInspiration {
    pub prompt_id: String,
    pub title: String,
    /// Already consent-filtered and fictionalized guidance from the event pack.
    pub guidance: String,
}

/// Server-derived identifiers that the model may reference. The response is
/// checked against this allowlist before it can become a rules-engine input.
#[derive(Debug, Clone, Serialize)]
pub struct LegalActionSet {
    pub character_id: String,
    pub skill_ids: Vec<String>,
    pub attack_ids: Vec<String>,
    pub target_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameMasterTurnContext {
    pub session: SessionDto,
    pub characters: Vec<Character>,
    pub recent_events: Vec<SessionEventDto>,
    pub player_intent: String,
    pub event_inspiration: Option<EventInspiration>,
    pub legal_actions: Vec<LegalActionSet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameMasterDraft {
    pub proposal: AiGmProposal,
    /// Digest of the exact validated proposal, including its unique ID. An
    /// acceptance audit stores this value to bind the decision to its content.
    pub proposal_fingerprint: Sha256Digest,
    /// Server-derived provenance for any private inspiration supplied to the
    /// model, even when the model omits an `introduce_event` effect.
    pub source_prompt_id: Option<String>,
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct PreparedGameMasterRequest {
    pub proposal_id: String,
    pub request: TextGenerationRequest,
}

#[derive(Clone)]
pub struct GameMasterService {
    generator: Arc<dyn TextGenerator>,
}

impl GameMasterService {
    pub fn new(generator: Arc<dyn TextGenerator>) -> Self {
        Self { generator }
    }

    pub fn build_request(
        &self,
        context: &GameMasterTurnContext,
    ) -> Result<PreparedGameMasterRequest, GameMasterError> {
        validate_context(context)?;
        let proposal_id = new_proposal_id(context)?;
        let envelope = RequestEnvelope {
            request_schema_version: REQUEST_SCHEMA_VERSION,
            task: "draft_next_turn",
            authoritative_context: context,
            required_output: RequiredOutput {
                proposal_schema_version: AI_PROPOSAL_SCHEMA_VERSION,
                proposal_id: &proposal_id,
                session_id: &context.session.id,
                based_on_event_sequence: context.session.last_event_sequence,
                shape: PROPOSAL_SHAPE,
            },
        };
        let content =
            serde_json::to_string(&envelope).map_err(GameMasterError::RequestSerialization)?;
        Ok(PreparedGameMasterRequest {
            proposal_id,
            request: TextGenerationRequest {
                messages: vec![
                    ChatMessage::system(SYSTEM_PROMPT),
                    ChatMessage::user(content),
                ],
                response_format: TextResponseFormat::JsonObject,
                temperature: None,
                max_output_tokens: None,
            },
        })
    }

    /// Produces an uncommitted draft. This method has no repository or mutable
    /// core-state dependency, so applying a proposal is necessarily a separate
    /// caller-controlled operation.
    pub async fn draft_turn(
        &self,
        context: &GameMasterTurnContext,
    ) -> Result<GameMasterDraft, GameMasterError> {
        let prepared = self.build_request(context)?;
        let response = self.generator.generate_text(prepared.request).await?;
        let mut proposal: AiGmProposal =
            serde_json::from_str(&response.text).map_err(GameMasterError::InvalidJson)?;
        // The provider's identifier is untrusted. Replace it with the stable,
        // server-owned id for this authoritative base sequence.
        proposal.proposal_id.clone_from(&prepared.proposal_id);
        validate_proposal(context, &proposal, &prepared.proposal_id)?;
        let serialized =
            serde_json::to_vec(&proposal).map_err(GameMasterError::ProposalSerialization)?;
        let digest: [u8; 32] = Sha256::digest(serialized).into();
        Ok(GameMasterDraft {
            proposal,
            proposal_fingerprint: Sha256Digest::from_bytes(digest),
            source_prompt_id: context
                .event_inspiration
                .as_ref()
                .map(|inspiration| inspiration.prompt_id.clone()),
            model: response.model,
            finish_reason: response.finish_reason,
            usage: response.usage,
        })
    }
}

#[derive(Serialize)]
struct RequestEnvelope<'a> {
    request_schema_version: u16,
    task: &'static str,
    authoritative_context: &'a GameMasterTurnContext,
    required_output: RequiredOutput<'a>,
}

#[derive(Serialize)]
struct RequiredOutput<'a> {
    proposal_schema_version: u16,
    proposal_id: &'a str,
    session_id: &'a str,
    based_on_event_sequence: u64,
    shape: &'static str,
}

const PROPOSAL_SHAPE: &str = r#"{"schema_version":1,"proposal_id":"non-empty unique id","session_id":"must match","based_on_event_sequence":0,"narrative":{"text":"...","image_prompt":null,"choices":[]},"effects":[{"type":"request_ability_check|request_attack|propose_reward|introduce_event|end_session", "...":"fields required by the selected tagged variant"}]}"#;

fn validate_context(context: &GameMasterTurnContext) -> Result<(), GameMasterError> {
    if context.session.validate().is_err() || context.session.status != SessionStatus::Active {
        return Err(GameMasterError::InvalidDraft(
            "authoritative session context is invalid".to_owned(),
        ));
    }
    if context.player_intent.trim().is_empty() || char_count(&context.player_intent) > 4_000 {
        return Err(GameMasterError::InvalidDraft(
            "player intent must contain between 1 and 4000 characters".to_owned(),
        ));
    }

    let session_character_ids = context
        .session
        .character_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let supplied_character_ids = context
        .characters
        .iter()
        .map(Character::id)
        .collect::<BTreeSet<_>>();
    if session_character_ids.len() != context.session.character_ids.len()
        || supplied_character_ids.len() != context.characters.len()
        || session_character_ids != supplied_character_ids
        || context
            .characters
            .iter()
            .any(|character| character.validate().is_err())
    {
        return Err(GameMasterError::InvalidDraft(
            "character context does not match the authoritative session".to_owned(),
        ));
    }
    if context.recent_events.len() > 100 || context.legal_actions.len() > 64 {
        return Err(GameMasterError::InvalidDraft(
            "turn context exceeds its collection limits".to_owned(),
        ));
    }
    let recent_sequences = context
        .recent_events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    if context.recent_events.iter().any(|event| {
        event.validate().is_err()
            || event.session_id != context.session.id
            || event.sequence > context.session.last_event_sequence
            || event_references_unknown_character(event, &session_character_ids)
    }) || recent_sequences.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(GameMasterError::InvalidDraft(
            "recent event context is inconsistent with the session".to_owned(),
        ));
    }
    if context.event_inspiration.as_ref().is_some_and(|event| {
        !is_valid_opaque_id(&event.prompt_id)
            || event.title.trim().is_empty()
            || char_count(&event.title) > 200
            || event.guidance.trim().is_empty()
            || char_count(&event.guidance) > 8_000
    }) {
        return Err(GameMasterError::InvalidDraft(
            "event inspiration is invalid".to_owned(),
        ));
    }
    let legal_character_ids = context
        .legal_actions
        .iter()
        .map(|legal| legal.character_id.as_str())
        .collect::<BTreeSet<_>>();
    if legal_character_ids.len() != context.legal_actions.len()
        || context.legal_actions.iter().any(|legal| {
            !supplied_character_ids.contains(legal.character_id.as_str())
                || !valid_reference_id(&legal.character_id)
                || !valid_id_list(&legal.skill_ids)
                || !valid_id_list(&legal.attack_ids)
                || !valid_id_list(&legal.target_ids)
        })
    {
        return Err(GameMasterError::InvalidDraft(
            "legal action context contains duplicate or unknown identifiers".to_owned(),
        ));
    }
    Ok(())
}

fn new_proposal_id(context: &GameMasterTurnContext) -> Result<String, GameMasterError> {
    let next_sequence = context
        .session
        .last_event_sequence
        .checked_add(1)
        .ok_or_else(|| GameMasterError::InvalidDraft("event sequence is exhausted".to_owned()))?;
    Ok(format!("gm:{next_sequence}:{}", Uuid::new_v4().simple()))
}

fn validate_proposal(
    context: &GameMasterTurnContext,
    proposal: &AiGmProposal,
    expected_proposal_id: &str,
) -> Result<(), GameMasterError> {
    if proposal.schema_version != AI_PROPOSAL_SCHEMA_VERSION {
        return Err(GameMasterError::InvalidDraft(format!(
            "proposal schema version must be {AI_PROPOSAL_SCHEMA_VERSION}"
        )));
    }
    if proposal.proposal_id != expected_proposal_id {
        return Err(GameMasterError::InvalidDraft(
            "proposal_id does not match the server-assigned id".to_owned(),
        ));
    }
    if proposal.session_id != context.session.id {
        return Err(GameMasterError::InvalidDraft(
            "proposal session_id does not match the authoritative session".to_owned(),
        ));
    }
    if proposal.based_on_event_sequence != context.session.last_event_sequence {
        return Err(GameMasterError::InvalidDraft(
            "proposal is based on a stale or future event sequence".to_owned(),
        ));
    }
    if proposal.narrative.as_ref().is_some_and(|narrative| {
        narrative.text.trim().is_empty()
            || char_count(&narrative.text) > MAX_NARRATIVE_CHARS
            || narrative.image_prompt.as_ref().is_some_and(|prompt| {
                prompt.trim().is_empty() || char_count(prompt) > MAX_IMAGE_PROMPT_CHARS
            })
            || narrative.choices.len() > 4
            || narrative
                .choices
                .iter()
                .any(|choice| choice.trim().is_empty() || char_count(choice) > 300)
    }) {
        return Err(GameMasterError::InvalidDraft(
            "narrative text and any image prompt must not be empty".to_owned(),
        ));
    }
    if proposal.effects.len() > MAX_EFFECTS {
        return Err(GameMasterError::InvalidDraft(format!(
            "proposal may contain at most {MAX_EFFECTS} effects"
        )));
    }
    for effect in &proposal.effects {
        let valid = match effect {
            ProposedEffect::RequestAbilityCheck {
                character_id,
                skill_id,
                reason,
                ..
            } => {
                valid_character_id(context, character_id)
                    && skill_id.as_ref().is_none_or(|skill| {
                        legal_actions_for(context, character_id)
                            .is_some_and(|legal| legal.skill_ids.contains(skill))
                    })
                    && valid_reason(reason)
            }
            ProposedEffect::RequestAttack {
                character_id,
                target_id,
                attack_id,
                reason,
                ..
            } => {
                valid_character_id(context, character_id)
                    && legal_actions_for(context, character_id).is_some_and(|legal| {
                        legal.target_ids.contains(target_id) && legal.attack_ids.contains(attack_id)
                    })
                    && valid_reason(reason)
            }
            ProposedEffect::ProposeReward {
                character_id,
                reason,
                ..
            } => valid_character_id(context, character_id) && valid_reason(reason),
            ProposedEffect::IntroduceEvent {
                title,
                description,
                source_prompt_id,
            } => {
                !title.trim().is_empty()
                    && char_count(title) <= 200
                    && !description.trim().is_empty()
                    && char_count(description) <= 5_000
                    && match (source_prompt_id, &context.event_inspiration) {
                        (None, None) => true,
                        (Some(source), Some(inspiration)) => source == &inspiration.prompt_id,
                        (None, Some(_)) | (Some(_), None) => false,
                    }
            }
            ProposedEffect::EndSession { reason } => valid_reason(reason),
        };
        if !valid {
            return Err(GameMasterError::InvalidDraft(
                "effect identifiers, descriptions, and reasons must not be empty".to_owned(),
            ));
        }
    }
    Ok(())
}

fn valid_character_id(context: &GameMasterTurnContext, id: &str) -> bool {
    context
        .characters
        .iter()
        .any(|character| character.id() == id)
}

fn event_references_unknown_character(event: &SessionEventDto, known: &BTreeSet<&str>) -> bool {
    use manchester_dnd_core::{EventActor, SessionEventPayload};

    if let EventActor::Player { character_id } = &event.actor
        && !known.contains(character_id.as_str())
    {
        return true;
    }
    match &event.payload {
        SessionEventPayload::PlayerIntent { character_id, .. }
        | SessionEventPayload::ExperienceAwarded { character_id, .. } => {
            !known.contains(character_id.as_str())
        }
        _ => false,
    }
}

fn legal_actions_for<'a>(
    context: &'a GameMasterTurnContext,
    character_id: &str,
) -> Option<&'a LegalActionSet> {
    context
        .legal_actions
        .iter()
        .find(|legal| legal.character_id == character_id)
}

fn valid_reference_id(value: &str) -> bool {
    is_valid_opaque_id(value)
}

fn valid_id_list(values: &[String]) -> bool {
    values.len() <= 256
        && values.iter().all(|value| valid_reference_id(value))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn valid_reason(reason: &str) -> bool {
    !reason.trim().is_empty() && char_count(reason) <= MAX_REASON_CHARS
}

fn char_count(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use manchester_dnd_core::{RULESET, SessionStatus};

    use super::*;
    use crate::{error::GenerationError, generation::TextGenerationResponse};

    struct StubGenerator {
        response: String,
        requests: Mutex<Vec<TextGenerationRequest>>,
    }

    #[async_trait]
    impl TextGenerator for StubGenerator {
        async fn generate_text(
            &self,
            request: TextGenerationRequest,
        ) -> Result<TextGenerationResponse, GenerationError> {
            self.requests.lock().expect("request lock").push(request);
            Ok(TextGenerationResponse {
                text: self.response.clone(),
                model: Some("stub-gm".to_owned()),
                finish_reason: Some("stop".to_owned()),
                usage: TokenUsage::default(),
            })
        }
    }

    fn context() -> GameMasterTurnContext {
        GameMasterTurnContext {
            session: SessionDto {
                schema_version: 1,
                id: "session-1".to_owned(),
                ruleset: RULESET,
                title: "Rain over Ancoats".to_owned(),
                status: SessionStatus::Active,
                character_ids: vec![],
                created_at_unix_ms: 1,
                updated_at_unix_ms: 2,
                last_event_sequence: 7,
            },
            characters: vec![],
            recent_events: vec![],
            player_intent: "Inspect the rune".to_owned(),
            event_inspiration: None,
            legal_actions: vec![],
        }
    }

    fn context_with_character() -> GameMasterTurnContext {
        use manchester_dnd_core::{AbilityScores, CharacterDraft};

        let character = CharacterDraft {
            id: "character-1".to_owned(),
            name: "Mara".to_owned(),
            theme: "Canal Warden".to_owned(),
            ability_scores: AbilityScores::new(10, 12, 12, 10, 14, 10).unwrap(),
            experience_points: 0,
            current_hit_points: 10,
            maximum_hit_points: 10,
        }
        .build()
        .unwrap();
        let mut context = context();
        context.session.character_ids = vec![character.id().to_owned()];
        context.characters = vec![character];
        context.legal_actions = vec![LegalActionSet {
            character_id: "character-1".to_owned(),
            skill_ids: vec!["perception".to_owned()],
            attack_ids: vec!["staff".to_owned()],
            target_ids: vec!["clockwork-rat".to_owned()],
        }];
        context
    }

    #[tokio::test]
    async fn returns_a_draft_without_mutating_the_input_context() {
        let generator = Arc::new(StubGenerator {
            response: serde_json::json!({
                "schema_version": 1,
                "proposal_id": "provider-chosen-id",
                "session_id": "session-1",
                "based_on_event_sequence": 7,
                "narrative": {"text": "The rune flickers.", "image_prompt": null},
                "effects": []
            })
            .to_string(),
            requests: Mutex::new(vec![]),
        });
        let service = GameMasterService::new(generator.clone());
        let context = context();

        let draft = service
            .draft_turn(&context)
            .await
            .expect("draft should parse");

        assert!(draft.proposal.proposal_id.starts_with("gm:8:"));
        assert!(draft.proposal_fingerprint.as_str().starts_with("sha256:"));
        assert_eq!(draft.source_prompt_id, None);
        assert_eq!(context.session.last_event_sequence, 7);
        let requests = generator.requests.lock().expect("request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].response_format, TextResponseFormat::JsonObject);
        assert!(
            requests[0].messages[1]
                .content
                .contains("authoritative_context")
        );
    }

    #[tokio::test]
    async fn rejects_a_stale_proposal() {
        let generator = Arc::new(StubGenerator {
            response: serde_json::json!({
                "schema_version": 1,
                "proposal_id": "gm:session-1:8",
                "session_id": "session-1",
                "based_on_event_sequence": 6,
                "narrative": null,
                "effects": []
            })
            .to_string(),
            requests: Mutex::new(vec![]),
        });
        let error = GameMasterService::new(generator)
            .draft_turn(&context())
            .await
            .expect_err("stale proposal must fail");

        assert!(matches!(error, GameMasterError::InvalidDraft(_)));
    }

    #[tokio::test]
    async fn rejects_attack_identifiers_outside_the_server_allowlist() {
        let generator = Arc::new(StubGenerator {
            response: serde_json::json!({
                "schema_version": 1,
                "proposal_id": "provider-id-is-replaced",
                "session_id": "session-1",
                "based_on_event_sequence": 7,
                "narrative": null,
                "effects": [{
                    "type": "request_attack",
                    "character_id": "character-1",
                    "target_id": "unknown-dragon",
                    "attack_id": "staff",
                    "reason": "The creature blocks the way"
                }]
            })
            .to_string(),
            requests: Mutex::new(vec![]),
        });
        let error = GameMasterService::new(generator)
            .draft_turn(&context_with_character())
            .await
            .expect_err("unknown targets must fail");

        assert!(matches!(error, GameMasterError::InvalidDraft(_)));
    }

    #[test]
    fn rejects_completed_session_contexts() {
        let generator = Arc::new(StubGenerator {
            response: String::new(),
            requests: Mutex::new(vec![]),
        });
        let service = GameMasterService::new(generator);
        let mut context = context();
        context.session.status = SessionStatus::Completed;

        let error = service
            .build_request(&context)
            .expect_err("a completed campaign cannot start another GM turn");
        assert!(matches!(error, GameMasterError::InvalidDraft(_)));
    }

    #[tokio::test]
    async fn draft_preserves_private_inspiration_provenance() {
        let generator = Arc::new(StubGenerator {
            response: serde_json::json!({
                "schema_version": 1,
                "proposal_id": "provider-id-is-replaced",
                "session_id": "session-1",
                "based_on_event_sequence": 7,
                "narrative": {"text": "A harmless echo crosses the canal."},
                "effects": []
            })
            .to_string(),
            requests: Mutex::new(vec![]),
        });
        let service = GameMasterService::new(generator);
        let mut context = context();
        context.event_inspiration = Some(EventInspiration {
            prompt_id: "rainy-picnic".to_owned(),
            title: "The Sudden Downpour".to_owned(),
            guidance: "Use only the abstract theme of plans changing.".to_owned(),
        });

        let draft = service
            .draft_turn(&context)
            .await
            .expect("valid draft should retain provenance");
        assert_eq!(draft.source_prompt_id.as_deref(), Some("rainy-picnic"));
    }

    #[tokio::test]
    async fn separate_draft_attempts_have_distinct_ids_and_fingerprints() {
        let generator = Arc::new(StubGenerator {
            response: serde_json::json!({
                "schema_version": 1,
                "proposal_id": "provider-id-is-replaced",
                "session_id": "session-1",
                "based_on_event_sequence": 7,
                "narrative": {"text": "The same candidate narration."},
                "effects": []
            })
            .to_string(),
            requests: Mutex::new(vec![]),
        });
        let service = GameMasterService::new(generator);

        let first = service.draft_turn(&context()).await.unwrap();
        let second = service.draft_turn(&context()).await.unwrap();

        assert_ne!(first.proposal.proposal_id, second.proposal.proposal_id);
        assert_ne!(first.proposal_fingerprint, second.proposal_fingerprint);
    }
}
