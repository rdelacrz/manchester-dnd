use std::{
    sync::{Arc, Mutex as StdMutex},
    time::{SystemTime, UNIX_EPOCH},
};

use manchester_dnd_core::{
    Ability, AbilityCheck, AbilityScores, AttemptExplorationCheckCommand, Character,
    CharacterDraft, DiceSource, EXPLORATION_CHECK_SCHEMA_VERSION, EventActor,
    ExplorationCheckOutcomeDto, LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION, LocalCampaignViewDto,
    Proficiency, RULESET, RollContext, SESSION_SCHEMA_VERSION, SessionDto, SessionEventDto,
    SessionEventPayload, SessionStatus, Sha256Digest,
};
use rand::Rng as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{
    config::AccessMode,
    error::{ApplicationError, RepositoryError},
    repository::{NewCommandReceipt, SqliteRepository, StoredDocument},
};

pub const LOCAL_CAMPAIGN_SESSION_ID: &str = "local-campaign";
pub const LOCAL_CHARACTER_ID: &str = "local-hero";
pub const LOCAL_EXPLORATION_ACTION_ID: &str = "inspect-viaduct-runes";

const LOCAL_CAMPAIGN_TITLE: &str = "The Runes Beneath the Viaduct";
const LOCAL_CHARACTER_NAME: &str = "Mara";
const LOCAL_CHARACTER_THEME: &str = "canal warden";
const EXPLORATION_COMMAND_KIND: &str = "attempt-exploration-check";

/// Supplies wall-clock timestamps while keeping application tests deterministic.
pub trait UnixTimeSource: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

impl<F> UnixTimeSource for F
where
    F: Fn() -> u64 + Send + Sync,
{
    fn now_unix_ms(&self) -> u64 {
        self()
    }
}

#[derive(Clone)]
pub struct GameApplicationService {
    access_mode: AccessMode,
    repository: SqliteRepository,
    dice: Arc<StdMutex<Box<dyn DiceSource + Send>>>,
    clock: Arc<dyn UnixTimeSource>,
    command_gate: Arc<AsyncMutex<()>>,
}

impl GameApplicationService {
    pub fn new(access_mode: AccessMode, repository: SqliteRepository) -> Self {
        Self::with_sources(access_mode, repository, SystemDice, SystemClock)
    }

    pub fn with_sources(
        access_mode: AccessMode,
        repository: SqliteRepository,
        dice: impl DiceSource + Send + 'static,
        clock: impl UnixTimeSource + 'static,
    ) -> Self {
        Self {
            access_mode,
            repository,
            dice: Arc::new(StdMutex::new(Box::new(dice))),
            clock: Arc::new(clock),
            command_gate: Arc::new(AsyncMutex::new(())),
        }
    }

    pub const fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    pub async fn health_check(&self) -> Result<(), ApplicationError> {
        self.repository
            .health_check()
            .await
            .map_err(ApplicationError::Repository)
    }

    /// Loads the one local campaign, creating its fixed level-one hero on the
    /// first request. Hosted access remains unavailable until authentication
    /// and campaign authorization are implemented.
    pub async fn load_local_campaign(&self) -> Result<LocalCampaignViewDto, ApplicationError> {
        self.require_local_mode()?;
        let _guard = self.command_gate.lock().await;
        let (session, character) = self.load_or_create_local_campaign().await?;
        self.build_local_view(&session, &character).await
    }

    /// Resolves the sole authored exploration action with server-owned rules,
    /// dice, timestamps, audit identity, and persistence.
    pub async fn attempt_exploration_check(
        &self,
        command: AttemptExplorationCheckCommand,
    ) -> Result<ExplorationCheckOutcomeDto, ApplicationError> {
        self.require_local_mode()?;
        command
            .validate()
            .map_err(ApplicationError::InvalidCommand)?;
        let fingerprint = fingerprint_command(&command)?;

        // Serializing local mutations ensures two duplicate requests cannot
        // both pass the receipt lookup and consume dice in this process.
        let _guard = self.command_gate.lock().await;
        let (stored_session, stored_character) = self.load_or_create_local_campaign().await?;

        if let Some(receipt) = self
            .repository
            .load_command_receipt(&command.campaign_session_id, &command.idempotency_key)
            .await
            .map_err(ApplicationError::Repository)?
        {
            return outcome_from_receipt(&command, &fingerprint, &receipt);
        }

        validate_local_command(&command, &stored_session, &stored_character)?;

        let next_revision = stored_session
            .revision
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let next_sequence = stored_session
            .value
            .last_event_sequence
            .checked_add(1)
            .ok_or(ApplicationError::InvalidStoredState)?;
        let occurred_at_unix_ms = self
            .clock
            .now_unix_ms()
            .max(stored_session.value.updated_at_unix_ms);

        let check = authored_exploration_check(&command.action_id)
            .ok_or_else(|| ApplicationError::UnknownAction(command.action_id.clone()))?;
        let result = {
            let mut dice = self
                .dice
                .lock()
                .map_err(|_| ApplicationError::InvalidStoredState)?;
            let mut dice = DynamicDice(&mut **dice);
            check
                .resolve(
                    stored_character.value.ability_scores(),
                    stored_character.value.level(),
                    &mut dice,
                )
                .map_err(ApplicationError::Rules)?
        };

        let outcome = ExplorationCheckOutcomeDto {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: command.campaign_session_id.clone(),
            character_id: command.character_id.clone(),
            action_id: command.action_id.clone(),
            result_revision: next_revision,
            event_sequence: next_sequence,
            result: result.clone(),
        };
        outcome
            .validate()
            .map_err(ApplicationError::InvalidOutcome)?;

        let mut post_session = stored_session.value.clone();
        post_session.updated_at_unix_ms = occurred_at_unix_ms;
        post_session.last_event_sequence = next_sequence;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: command.campaign_session_id.clone(),
            sequence: next_sequence,
            occurred_at_unix_ms,
            actor: EventActor::System,
            payload: SessionEventPayload::AbilityCheckResolved {
                character_id: command.character_id.clone(),
                action_id: command.action_id.clone(),
                result,
            },
        };
        event.validate().map_err(ApplicationError::InvalidOutcome)?;

        let audit_id = format!("turn:{}", Uuid::new_v4().simple());
        let response_json =
            serde_json::to_string(&outcome).map_err(ApplicationError::Serialization)?;
        let receipt = NewCommandReceipt {
            campaign_session_id: command.campaign_session_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            command_kind: EXPLORATION_COMMAND_KIND.to_owned(),
            request_fingerprint: fingerprint,
            expected_revision: command.expected_revision,
            result_revision: next_revision,
            audit_id: audit_id.clone(),
            response_json,
        };

        match self
            .repository
            .commit_session_event_with_receipt(
                &audit_id,
                &post_session,
                command.expected_revision,
                &event,
                &[],
                &receipt,
            )
            .await
        {
            Ok(committed) if committed.session.revision == next_revision => Ok(outcome),
            Ok(_) => Err(ApplicationError::InvalidStoredState),
            Err(RepositoryError::RevisionConflict { actual, .. }) => {
                Err(ApplicationError::RevisionConflict {
                    expected: command.expected_revision,
                    current_revision: actual,
                })
            }
            Err(RepositoryError::AlreadyExists {
                entity: "command receipt",
                ..
            }) => {
                let stored = self
                    .repository
                    .load_command_receipt(&command.campaign_session_id, &command.idempotency_key)
                    .await
                    .map_err(ApplicationError::Repository)?
                    .ok_or(ApplicationError::InvalidStoredState)?;
                outcome_from_receipt(&command, &receipt.request_fingerprint, &stored)
            }
            Err(error) => Err(ApplicationError::Repository(error)),
        }
    }

    fn require_local_mode(&self) -> Result<(), ApplicationError> {
        match self.access_mode {
            AccessMode::LocalSingleUser => Ok(()),
            AccessMode::Hosted => Err(ApplicationError::HostedAccessDenied),
        }
    }

    async fn load_or_create_local_campaign(
        &self,
    ) -> Result<(StoredDocument<SessionDto>, StoredDocument<Character>), ApplicationError> {
        if self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .is_none()
        {
            let now = self.clock.now_unix_ms();
            let session = local_session(now);
            let character = local_character()?;
            match self
                .repository
                .create_campaign(&session, std::slice::from_ref(&character))
                .await
            {
                Ok(_) | Err(RepositoryError::AlreadyExists { .. }) => {}
                Err(error) => return Err(ApplicationError::Repository(error)),
            }
        }

        let session = self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::InvalidStoredState)?;
        let character = self
            .repository
            .load_character(LOCAL_CHARACTER_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .ok_or(ApplicationError::InvalidStoredState)?;
        validate_local_documents(&session, &character)?;
        Ok((session, character))
    }

    async fn build_local_view(
        &self,
        session: &StoredDocument<SessionDto>,
        character: &StoredDocument<Character>,
    ) -> Result<LocalCampaignViewDto, ApplicationError> {
        let events = self
            .repository
            .list_session_events(&session.id)
            .await
            .map_err(ApplicationError::Repository)?;
        let latest_check = events.iter().rev().find_map(|audit| {
            let SessionEventPayload::AbilityCheckResolved {
                character_id,
                action_id,
                result,
            } = &audit.payload.payload
            else {
                return None;
            };
            Some(ExplorationCheckOutcomeDto {
                schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
                campaign_session_id: audit.campaign_session_id.clone(),
                character_id: character_id.clone(),
                action_id: action_id.clone(),
                // Campaign creation is revision one and every subsequent
                // revision has exactly one corresponding ordered event.
                result_revision: audit.turn_number.checked_add(1)?,
                event_sequence: audit.turn_number,
                result: result.clone(),
            })
        });
        if latest_check
            .as_ref()
            .is_some_and(|outcome| outcome.validate().is_err())
        {
            return Err(ApplicationError::InvalidStoredState);
        }

        let view = LocalCampaignViewDto {
            schema_version: LOCAL_CAMPAIGN_VIEW_SCHEMA_VERSION,
            campaign_session_id: session.id.clone(),
            character_id: character.id.clone(),
            campaign_title: session.value.title.clone(),
            character_name: character.value.name().to_owned(),
            revision: session.revision,
            last_event_sequence: session.value.last_event_sequence,
            latest_check,
        };
        view.validate().map_err(ApplicationError::InvalidOutcome)?;
        Ok(view)
    }
}

#[derive(Serialize)]
struct NormalizedExplorationCommand<'a> {
    schema_version: u16,
    campaign_session_id: &'a str,
    character_id: &'a str,
    action_id: &'a str,
    expected_revision: u64,
}

fn fingerprint_command(
    command: &AttemptExplorationCheckCommand,
) -> Result<Sha256Digest, ApplicationError> {
    let normalized = NormalizedExplorationCommand {
        schema_version: command.schema_version,
        campaign_session_id: &command.campaign_session_id,
        character_id: &command.character_id,
        action_id: &command.action_id,
        expected_revision: command.expected_revision,
    };
    let serialized = serde_json::to_vec(&normalized).map_err(ApplicationError::Serialization)?;
    let digest: [u8; 32] = Sha256::digest(serialized).into();
    Ok(Sha256Digest::from_bytes(digest))
}

fn outcome_from_receipt(
    command: &AttemptExplorationCheckCommand,
    fingerprint: &Sha256Digest,
    receipt: &crate::repository::StoredCommandReceipt,
) -> Result<ExplorationCheckOutcomeDto, ApplicationError> {
    if &receipt.request_fingerprint != fingerprint {
        return Err(ApplicationError::IdempotencyConflict);
    }
    if receipt.command_kind != EXPLORATION_COMMAND_KIND
        || receipt.campaign_session_id != command.campaign_session_id
        || receipt.idempotency_key != command.idempotency_key
        || receipt.expected_revision != command.expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    let outcome: ExplorationCheckOutcomeDto =
        serde_json::from_str(&receipt.response_json).map_err(ApplicationError::StoredResponse)?;
    if outcome.campaign_session_id != command.campaign_session_id
        || outcome.character_id != command.character_id
        || outcome.action_id != command.action_id
        || outcome.result_revision != receipt.result_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(outcome)
}

fn validate_local_command(
    command: &AttemptExplorationCheckCommand,
    session: &StoredDocument<SessionDto>,
    character: &StoredDocument<Character>,
) -> Result<(), ApplicationError> {
    if command.campaign_session_id != session.id {
        return Err(ApplicationError::WrongCampaign);
    }
    if command.character_id != character.id {
        return Err(ApplicationError::WrongCharacter);
    }
    if authored_exploration_check(&command.action_id).is_none() {
        return Err(ApplicationError::UnknownAction(command.action_id.clone()));
    }
    if session.value.status != SessionStatus::Active {
        return Err(ApplicationError::CampaignCompleted);
    }
    if command.expected_revision != session.revision {
        return Err(ApplicationError::RevisionConflict {
            expected: command.expected_revision,
            current_revision: session.revision,
        });
    }
    Ok(())
}

fn validate_local_documents(
    session: &StoredDocument<SessionDto>,
    character: &StoredDocument<Character>,
) -> Result<(), ApplicationError> {
    let expected_revision = session
        .value
        .last_event_sequence
        .checked_add(1)
        .ok_or(ApplicationError::InvalidStoredState)?;
    if session.id != LOCAL_CAMPAIGN_SESSION_ID
        || character.id != LOCAL_CHARACTER_ID
        || session.value.character_ids.as_slice() != [LOCAL_CHARACTER_ID]
        || session.revision != expected_revision
    {
        return Err(ApplicationError::InvalidStoredState);
    }
    Ok(())
}

fn authored_exploration_check(action_id: &str) -> Option<AbilityCheck> {
    match action_id {
        LOCAL_EXPLORATION_ACTION_ID => Some(AbilityCheck {
            ability: Ability::Wisdom,
            proficiency: Proficiency::Proficient,
            difficulty_class: 13,
            situational_modifier: 0,
            roll_context: RollContext::normal(),
        }),
        _ => None,
    }
}

fn local_session(now_unix_ms: u64) -> SessionDto {
    SessionDto {
        schema_version: SESSION_SCHEMA_VERSION,
        id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
        ruleset: RULESET,
        title: LOCAL_CAMPAIGN_TITLE.to_owned(),
        status: SessionStatus::Active,
        character_ids: vec![LOCAL_CHARACTER_ID.to_owned()],
        created_at_unix_ms: now_unix_ms,
        updated_at_unix_ms: now_unix_ms,
        last_event_sequence: 0,
    }
}

fn local_character() -> Result<Character, ApplicationError> {
    CharacterDraft {
        id: LOCAL_CHARACTER_ID.to_owned(),
        name: LOCAL_CHARACTER_NAME.to_owned(),
        theme: LOCAL_CHARACTER_THEME.to_owned(),
        ability_scores: AbilityScores::new(10, 12, 10, 10, 14, 10)
            .map_err(ApplicationError::InvalidOutcome)?,
        experience_points: 0,
        current_hit_points: 10,
        maximum_hit_points: 10,
    }
    .build()
    .map_err(ApplicationError::InvalidOutcome)
}

struct SystemDice;

impl DiceSource for SystemDice {
    fn roll(&mut self, sides: u16) -> u16 {
        rand::rng().random_range(1..=sides)
    }
}

struct DynamicDice<'a>(&'a mut (dyn DiceSource + Send));

impl DiceSource for DynamicDice<'_> {
    fn roll(&mut self, sides: u16) -> u16 {
        self.0.roll(sides)
    }
}

struct SystemClock;

impl UnixTimeSource for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    async fn test_service(
        access_mode: AccessMode,
    ) -> (GameApplicationService, SqliteRepository, Arc<AtomicUsize>) {
        let repository = SqliteRepository::connect("sqlite::memory:")
            .await
            .expect("test repository should initialize");
        let roll_count = Arc::new(AtomicUsize::new(0));
        let observed_rolls = roll_count.clone();
        let dice = move |sides| {
            assert_eq!(sides, 20);
            observed_rolls.fetch_add(1, Ordering::SeqCst);
            12
        };
        let service =
            GameApplicationService::with_sources(access_mode, repository.clone(), dice, || 1_000);
        (service, repository, roll_count)
    }

    fn command(view: &LocalCampaignViewDto, key: &str) -> AttemptExplorationCheckCommand {
        AttemptExplorationCheckCommand {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: view.campaign_session_id.clone(),
            character_id: view.character_id.clone(),
            action_id: LOCAL_EXPLORATION_ACTION_ID.to_owned(),
            expected_revision: view.revision,
            idempotency_key: key.to_owned(),
        }
    }

    #[tokio::test]
    async fn local_campaign_is_created_once_and_resumed() {
        let (service, repository, _) = test_service(AccessMode::LocalSingleUser).await;
        let first = service.load_local_campaign().await.unwrap();
        let resumed = GameApplicationService::with_sources(
            AccessMode::LocalSingleUser,
            repository.clone(),
            |_| 20,
            || 2_000,
        )
        .load_local_campaign()
        .await
        .unwrap();

        assert_eq!(first, resumed);
        assert_eq!(first.revision, 1);
        assert_eq!(first.last_event_sequence, 0);
        let character = repository
            .load_character(LOCAL_CHARACTER_ID)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(character.value.level().value(), 1);
    }

    #[tokio::test]
    async fn deterministic_check_commits_and_reloads_without_rerolling() {
        let (service, repository, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        let outcome = service
            .attempt_exploration_check(command(&view, "check-1"))
            .await
            .unwrap();

        assert_eq!(outcome.result.roll.selected, 12);
        assert_eq!(outcome.result.ability, Ability::Wisdom);
        assert_eq!(outcome.result.ability_modifier, 2);
        assert_eq!(outcome.result.proficiency_modifier, 2);
        assert_eq!(outcome.result.difficulty_class, 13);
        assert_eq!(outcome.result.total, 16);
        assert!(outcome.result.success);
        assert_eq!(outcome.result_revision, 2);
        assert_eq!(outcome.event_sequence, 1);

        let reloaded = service.load_local_campaign().await.unwrap();
        assert_eq!(reloaded.latest_check, Some(outcome));
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn matching_receipt_replays_before_revision_validation_without_rerolling() {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        let request = command(&view, "check-replay");

        let first = service
            .attempt_exploration_check(request.clone())
            .await
            .unwrap();
        let replay = service.attempt_exploration_check(request).await.unwrap();

        assert_eq!(replay, first);
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_duplicate_requests_share_one_roll_and_commit() {
        let (service, repository, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        let request = command(&view, "check-concurrent");
        let first_service = service.clone();
        let second_service = service.clone();
        let first_request = request.clone();

        let (first, second) = tokio::join!(
            first_service.attempt_exploration_check(first_request),
            second_service.attempt_exploration_check(request),
        );

        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .list_session_events(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn stale_revision_is_rejected_before_dice_are_consumed() {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        service
            .attempt_exploration_check(command(&view, "check-first"))
            .await
            .unwrap();
        let error = service
            .attempt_exploration_check(command(&view, "check-stale"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ApplicationError::RevisionConflict {
                expected: 1,
                current_revision: 2
            }
        ));
        assert_eq!(error.current_revision(), Some(2));
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn idempotency_key_cannot_be_reused_for_a_different_command() {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        let original = command(&view, "same-key");
        service
            .attempt_exploration_check(original.clone())
            .await
            .unwrap();
        let mut changed = original;
        changed.action_id = "another-action".to_owned();

        assert!(matches!(
            service.attempt_exploration_check(changed).await,
            Err(ApplicationError::IdempotencyConflict)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_action_and_wrong_character_are_rejected() {
        let (service, _, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        let mut wrong_action = command(&view, "wrong-action");
        wrong_action.action_id = "search-somewhere-else".to_owned();
        assert!(matches!(
            service.attempt_exploration_check(wrong_action).await,
            Err(ApplicationError::UnknownAction(_))
        ));

        let mut wrong_character = command(&view, "wrong-character");
        wrong_character.character_id = "another-character".to_owned();
        assert!(matches!(
            service.attempt_exploration_check(wrong_character).await,
            Err(ApplicationError::WrongCharacter)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn completed_campaign_rejects_new_checks() {
        let (service, repository, roll_count) = test_service(AccessMode::LocalSingleUser).await;
        let view = service.load_local_campaign().await.unwrap();
        let stored = repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .unwrap()
            .unwrap();
        let mut completed = stored.value;
        completed.status = SessionStatus::Completed;
        completed.last_event_sequence = 1;
        completed.updated_at_unix_ms = 1_001;
        let event = SessionEventDto {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            sequence: 1,
            occurred_at_unix_ms: 1_001,
            actor: EventActor::System,
            payload: SessionEventPayload::SessionEnded,
        };
        repository
            .commit_session_event("test-end", &completed, 1, &event, &[])
            .await
            .unwrap();
        let mut request = command(&view, "after-end");
        request.expected_revision = 2;

        assert!(matches!(
            service.attempt_exploration_check(request).await,
            Err(ApplicationError::CampaignCompleted)
        ));
        assert_eq!(roll_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn hosted_mode_fails_closed_for_loads_and_mutations() {
        let (service, repository, _) = test_service(AccessMode::Hosted).await;

        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        let request = AttemptExplorationCheckCommand {
            schema_version: EXPLORATION_CHECK_SCHEMA_VERSION,
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            character_id: LOCAL_CHARACTER_ID.to_owned(),
            action_id: LOCAL_EXPLORATION_ACTION_ID.to_owned(),
            expected_revision: 1,
            idempotency_key: "hosted-denied".to_owned(),
        };
        assert!(matches!(
            service.attempt_exploration_check(request).await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        assert!(
            repository
                .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
                .await
                .unwrap()
                .is_none()
        );
    }
}
