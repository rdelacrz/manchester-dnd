//! Local-owner application boundary for campaign lifecycle operations.
//!
//! This module deliberately binds every operation to the compiled local owner
//! and campaign IDs. Hosted mode remains denied by `require_local_mode`.

use manchester_dnd_core::is_valid_opaque_id;

use super::{
    GameApplicationService, LOCAL_CAMPAIGN_SESSION_ID, LOCAL_HERO_OWNER_KEY, local_character,
    local_session,
};
use crate::{
    error::{ApplicationError, RepositoryError},
    repository::{
        CampaignLifecycleCommand, CampaignLifecycleOutcome, CampaignLifecycleState,
        CampaignPrivateExportV1, CampaignPrivateRecap, CampaignSummary, CampaignTurnHistoryPage,
        DeleteCampaignCommand, EndPlaySessionCommand, GeneratePrivateRecapCommand,
        PreparedCampaignDeletion, RestoreCampaignExportCommand, StartPlaySessionCommand,
    },
};

impl GameApplicationService {
    /// Lists zero or one fixed-local campaigns. This does not implicitly create
    /// a replacement after an explicit owner deletion.
    pub async fn list_local_campaigns(&self) -> Result<Vec<CampaignSummary>, ApplicationError> {
        self.require_local_mode()?;
        self.repository
            .list_owned_campaigns(LOCAL_HERO_OWNER_KEY)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    /// Explicitly creates the fixed local campaign if it is absent. The stable
    /// primary key makes an interrupted/replayed create naturally idempotent.
    pub async fn create_local_campaign(&self) -> Result<CampaignSummary, ApplicationError> {
        self.require_local_mode()?;
        let _guard = self.command_gate.lock().await;
        if self
            .repository
            .load_campaign_session(LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
            .is_none()
        {
            self.repository
                .retire_deleted_campaign_receipts_for_recreate(
                    LOCAL_HERO_OWNER_KEY,
                    LOCAL_CAMPAIGN_SESSION_ID,
                )
                .await
                .map_err(ApplicationError::Repository)?;
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
        self.repository
            .load_owned_campaign_summary(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(map_lifecycle_repository_error)?
            .ok_or(ApplicationError::WrongCampaign)
    }

    pub async fn start_local_play_session(
        &self,
        command: StartPlaySessionCommand,
    ) -> Result<CampaignLifecycleOutcome, ApplicationError> {
        self.require_local_mode()?;
        require_fixed_campaign(&command.lifecycle.campaign_session_id)?;
        let _guard = self.command_gate.lock().await;
        let summary = self.require_local_campaign_summary().await?;
        if summary.lifecycle_state == CampaignLifecycleState::Archived {
            return Err(ApplicationError::CampaignArchived);
        }
        if summary.open_play_session_id.is_some() {
            return Err(ApplicationError::CampaignPlaySessionConflict);
        }
        self.repository
            .start_campaign_play_session(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn end_local_play_session(
        &self,
        command: EndPlaySessionCommand,
    ) -> Result<CampaignLifecycleOutcome, ApplicationError> {
        self.require_local_mode()?;
        require_fixed_campaign(&command.lifecycle.campaign_session_id)?;
        let _guard = self.command_gate.lock().await;
        let summary = self.require_local_campaign_summary().await?;
        if summary.open_play_session_id.as_deref() != Some(&command.play_session_id) {
            return Err(ApplicationError::CampaignPlaySessionConflict);
        }
        self.repository
            .end_campaign_play_session(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn archive_local_campaign(
        &self,
        command: CampaignLifecycleCommand,
    ) -> Result<CampaignLifecycleOutcome, ApplicationError> {
        self.require_local_mode()?;
        require_fixed_campaign(&command.campaign_session_id)?;
        let _guard = self.command_gate.lock().await;
        let summary = self.require_local_campaign_summary().await?;
        if summary.lifecycle_state == CampaignLifecycleState::Archived {
            return Err(ApplicationError::CampaignArchived);
        }
        if summary.open_play_session_id.is_some() {
            return Err(ApplicationError::CampaignPlaySessionConflict);
        }
        self.repository
            .archive_campaign(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn restore_local_campaign_from_archive(
        &self,
        command: CampaignLifecycleCommand,
    ) -> Result<CampaignLifecycleOutcome, ApplicationError> {
        self.require_local_mode()?;
        require_fixed_campaign(&command.campaign_session_id)?;
        let _guard = self.command_gate.lock().await;
        let summary = self.require_local_campaign_summary().await?;
        if summary.lifecycle_state != CampaignLifecycleState::Archived {
            return Err(ApplicationError::CampaignNotArchived);
        }
        self.repository
            .restore_archived_campaign(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn local_campaign_history(
        &self,
        after_turn_number: Option<u64>,
        limit: u16,
    ) -> Result<CampaignTurnHistoryPage, ApplicationError> {
        self.require_local_mode()?;
        self.repository
            .list_campaign_turn_history(
                LOCAL_HERO_OWNER_KEY,
                LOCAL_CAMPAIGN_SESSION_ID,
                after_turn_number,
                limit,
            )
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn export_local_campaign_private(
        &self,
    ) -> Result<CampaignPrivateExportV1, ApplicationError> {
        self.require_local_mode()?;
        self.repository
            .export_campaign_private(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(map_export_repository_error)
    }

    pub async fn export_local_campaign_canonical_json(&self) -> Result<String, ApplicationError> {
        self.require_local_mode()?;
        self.repository
            .export_campaign_canonical_json(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(map_export_repository_error)
    }

    pub async fn export_local_campaign_player_readable(&self) -> Result<String, ApplicationError> {
        self.require_local_mode()?;
        self.repository
            .export_campaign_player_readable(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(map_export_repository_error)
    }

    pub async fn generate_local_private_recap(
        &self,
        command: GeneratePrivateRecapCommand,
    ) -> Result<CampaignPrivateRecap, ApplicationError> {
        self.require_local_mode()?;
        require_fixed_campaign(&command.campaign_session_id)?;
        let _guard = self.command_gate.lock().await;
        self.repository
            .generate_private_recap(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_recap_repository_error)
    }

    pub async fn load_local_private_recap(
        &self,
    ) -> Result<Option<CampaignPrivateRecap>, ApplicationError> {
        self.require_local_mode()?;
        self.repository
            .load_latest_private_recap(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(map_recap_repository_error)
    }

    /// Produces the canonical owner export and binds its server-computed digest
    /// to a short-lived opaque deletion preparation.
    pub async fn prepare_local_campaign_deletion(
        &self,
        expected_lifecycle_revision: u64,
        deletion_id: String,
    ) -> Result<PreparedCampaignDeletion, ApplicationError> {
        self.require_local_mode()?;
        if !is_valid_opaque_id(&deletion_id) {
            return Err(ApplicationError::InvalidCampaignLifecycle);
        }
        let _guard = self.command_gate.lock().await;
        let summary = self.require_local_campaign_summary().await?;
        if summary.lifecycle_state != CampaignLifecycleState::Archived {
            return Err(ApplicationError::CampaignNotArchived);
        }
        self.repository
            .prepare_campaign_deletion(
                LOCAL_HERO_OWNER_KEY,
                LOCAL_CAMPAIGN_SESSION_ID,
                expected_lifecycle_revision,
                &deletion_id,
            )
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn delete_local_campaign(
        &self,
        command: DeleteCampaignCommand,
    ) -> Result<CampaignLifecycleOutcome, ApplicationError> {
        self.require_local_mode()?;
        require_fixed_campaign(&command.lifecycle.campaign_session_id)?;
        let _guard = self.command_gate.lock().await;
        self.repository
            .delete_archived_campaign(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_lifecycle_repository_error)
    }

    pub async fn restore_local_campaign_export(
        &self,
        command: RestoreCampaignExportCommand,
    ) -> Result<CampaignLifecycleOutcome, ApplicationError> {
        self.require_local_mode()?;
        let _guard = self.command_gate.lock().await;
        self.repository
            .restore_campaign_export(LOCAL_HERO_OWNER_KEY, &command)
            .await
            .map_err(map_export_repository_error)
    }

    pub(super) async fn require_local_campaign_active(&self) -> Result<(), ApplicationError> {
        let summary = self.require_local_campaign_summary().await?;
        if summary.lifecycle_state == CampaignLifecycleState::Archived {
            return Err(ApplicationError::CampaignArchived);
        }
        Ok(())
    }

    pub(super) async fn explicit_delete_prevents_implicit_recreation(
        &self,
    ) -> Result<(), ApplicationError> {
        if self
            .repository
            .has_campaign_deletion_tombstone(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(ApplicationError::Repository)?
        {
            return Err(ApplicationError::WrongCampaign);
        }
        Ok(())
    }

    async fn require_local_campaign_summary(&self) -> Result<CampaignSummary, ApplicationError> {
        self.repository
            .load_owned_campaign_summary(LOCAL_HERO_OWNER_KEY, LOCAL_CAMPAIGN_SESSION_ID)
            .await
            .map_err(map_lifecycle_repository_error)?
            .ok_or(ApplicationError::WrongCampaign)
    }
}

fn require_fixed_campaign(campaign_session_id: &str) -> Result<(), ApplicationError> {
    if campaign_session_id != LOCAL_CAMPAIGN_SESSION_ID {
        return Err(ApplicationError::WrongCampaign);
    }
    Ok(())
}

fn map_lifecycle_repository_error(error: RepositoryError) -> ApplicationError {
    match error {
        RepositoryError::RevisionConflict {
            expected, actual, ..
        } => ApplicationError::LifecycleRevisionConflict {
            expected,
            current_revision: actual,
        },
        RepositoryError::NotFound { .. } => ApplicationError::WrongCampaign,
        RepositoryError::InvalidDomainState { .. }
        | RepositoryError::UnsupportedSchemaVersion { .. }
        | RepositoryError::AlreadyExists { .. } => ApplicationError::InvalidCampaignLifecycle,
        other => ApplicationError::Repository(other),
    }
}

fn map_export_repository_error(error: RepositoryError) -> ApplicationError {
    match error {
        RepositoryError::NotFound { .. } => ApplicationError::WrongCampaign,
        RepositoryError::InvalidStoredData { .. }
        | RepositoryError::InvalidDomainState { .. }
        | RepositoryError::UnsupportedSchemaVersion { .. }
        | RepositoryError::IdentityMismatch { .. }
        | RepositoryError::CoreValidation { .. }
        | RepositoryError::HeroValidation { .. }
        | RepositoryError::AlreadyExists { .. } => ApplicationError::InvalidCampaignExport,
        RepositoryError::RevisionConflict {
            expected, actual, ..
        } => ApplicationError::LifecycleRevisionConflict {
            expected,
            current_revision: actual,
        },
        other => ApplicationError::Repository(other),
    }
}

fn map_recap_repository_error(error: RepositoryError) -> ApplicationError {
    match error {
        RepositoryError::NotFound { .. } => ApplicationError::WrongCampaign,
        RepositoryError::RevisionConflict {
            expected, actual, ..
        } => ApplicationError::RevisionConflict {
            expected,
            current_revision: actual,
        },
        RepositoryError::InvalidDomainState { .. }
        | RepositoryError::UnsupportedSchemaVersion { .. } => {
            ApplicationError::InvalidCampaignLifecycle
        }
        other => ApplicationError::Repository(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::{config::AccessMode, repository::MIGRATOR, seed::SeedVault};

    fn service(pool: PgPool, access_mode: AccessMode) -> GameApplicationService {
        let repository = crate::repository::PostgresRepository::from_pool(pool);
        GameApplicationService::with_sources(
            access_mode,
            repository,
            Arc::new(SeedVault::from_key([9; 32])),
            |_| 10,
            || 1_000,
        )
    }

    fn lifecycle(expected: u64, key: &str) -> CampaignLifecycleCommand {
        CampaignLifecycleCommand {
            schema_version: crate::repository::CAMPAIGN_LIFECYCLE_SCHEMA_VERSION,
            campaign_session_id: LOCAL_CAMPAIGN_SESSION_ID.to_owned(),
            expected_lifecycle_revision: expected,
            idempotency_key: key.to_owned(),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn local_application_lists_creates_archives_deletes_and_never_recreates_implicitly(
        pool: PgPool,
    ) {
        let service = service(pool.clone(), AccessMode::LocalSingleUser);
        assert!(service.list_local_campaigns().await.unwrap().is_empty());
        let created = service.create_local_campaign().await.unwrap();
        assert_eq!(created.lifecycle_revision, 1);
        assert!(service.load_local_campaign().await.is_ok());

        service
            .start_local_play_session(StartPlaySessionCommand {
                lifecycle: lifecycle(1, "app-start"),
                play_session_id: "app-play".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(
            service
                .archive_local_campaign(lifecycle(2, "archive-open"))
                .await,
            Err(ApplicationError::CampaignPlaySessionConflict)
        ));
        service
            .end_local_play_session(EndPlaySessionCommand {
                lifecycle: lifecycle(2, "app-end"),
                play_session_id: "app-play".to_owned(),
            })
            .await
            .unwrap();
        service
            .archive_local_campaign(lifecycle(3, "app-archive"))
            .await
            .unwrap();
        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::CampaignArchived)
        ));
        assert!(service.export_local_campaign_canonical_json().await.is_ok());
        service
            .restore_local_campaign_from_archive(lifecycle(4, "app-restore"))
            .await
            .unwrap();
        assert!(service.load_local_campaign().await.is_ok());
        service
            .archive_local_campaign(lifecycle(5, "app-rearchive"))
            .await
            .unwrap();
        let prepared = service
            .prepare_local_campaign_deletion(6, "app-delete-preparation".to_owned())
            .await
            .unwrap();
        service
            .delete_local_campaign(DeleteCampaignCommand {
                lifecycle: lifecycle(6, "app-delete"),
                deletion_id: prepared.deletion_id,
                confirm_permanent_delete: true,
            })
            .await
            .unwrap();
        assert!(matches!(
            service.load_local_campaign().await,
            Err(ApplicationError::WrongCampaign)
        ));
        assert!(service.list_local_campaigns().await.unwrap().is_empty());

        let recreated = service.create_local_campaign().await.unwrap();
        assert_eq!(recreated.lifecycle_revision, 1);
        assert!(service.load_local_campaign().await.is_ok());
        let old_receipts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM campaign_lifecycle_receipts
             WHERE owner_key = $1 AND campaign_session_id = $2",
        )
        .bind(LOCAL_HERO_OWNER_KEY)
        .bind(LOCAL_CAMPAIGN_SESSION_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            old_receipts, 0,
            "new local campaign is a fresh idempotency scope"
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn hosted_mode_denies_every_lifecycle_entrypoint(pool: PgPool) {
        let service = service(pool, AccessMode::Hosted);
        assert!(matches!(
            service.list_local_campaigns().await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        assert!(matches!(
            service.create_local_campaign().await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        assert!(matches!(
            service
                .start_local_play_session(StartPlaySessionCommand {
                    lifecycle: lifecycle(1, "hosted-start"),
                    play_session_id: "hosted-play".to_owned(),
                })
                .await,
            Err(ApplicationError::HostedAccessDenied)
        ));
        assert!(matches!(
            service
                .restore_local_campaign_export(RestoreCampaignExportCommand {
                    schema_version: crate::repository::CAMPAIGN_EXPORT_SCHEMA_VERSION,
                    idempotency_key: "hosted-restore".to_owned(),
                    canonical_export_json: "{}".to_owned(),
                })
                .await,
            Err(ApplicationError::HostedAccessDenied)
        ));
    }
}
