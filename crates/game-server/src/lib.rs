//! Server-side boundaries for configuration, generation, persistence, event
//! prompts, and AI game-master orchestration.
//!
//! The rules engine remains in `manchester-dnd-core`. In particular, the GM
//! service in this crate only returns proposals; callers must validate and
//! apply them through the authoritative core domain.

pub mod application;
pub mod auth;
pub mod campaign_pins;
pub mod config;
pub mod content;
pub mod context;
pub mod error;
pub mod events;
pub mod generation;
pub mod generation_ledger;
pub mod gm;
pub mod inspiration;
pub mod recovery_vault;
pub mod repository;
pub mod scene_images;
pub mod seed;
pub mod source_vault;
pub mod typed_gm;

pub use application::{
    ClaimEncounterRewardCommand, EncounterRewardClaimOutcomeDto, GameApplicationService,
    HERO_APPLICATION_SCHEMA_VERSION, HERO_DRAFT_RETENTION_SECONDS, HERO_DRAFT_TTL_SECONDS,
    HeroLevelUpChoicesDto, HeroLevelUpOutcomeDto, HeroRewardOutcomeDto, LOCAL_CAMPAIGN_SESSION_ID,
    LOCAL_CHARACTER_ID, LOCAL_EXPLORATION_ACTION_ID, LOCAL_HERO_OWNER_KEY, LOCAL_SOCIAL_ACTION_ID,
    LocalHeroWorkspaceDto, UnixTimeSource,
};
pub use auth::{
    AccountPrincipal, AccountSummary, AuthService, AuthenticatedSession, AuthenticationActionKind,
    AuthenticationAudit, AuthenticationInputError, AuthenticationSecret,
    AuthenticationThrottleBucket, IssuedSession, LOCAL_ACCOUNT_ID, PasswordPhc,
};
pub use campaign_pins::{CampaignPinRuntime, CampaignPinValidationError};
pub use config::{
    AccessMode, AppConfig, AuthenticationConfig, ContentPackConfig, DatabaseRuntimeConfig,
    GenerationConfigFingerprints, LlmBackend, LlmProfile, SecretString,
};
pub use content::{ActiveContentCatalog, ActiveContentPack, ContentCatalogError};
pub use context::ServerContext;
pub use error::{
    ApplicationError, AuthenticationError, BootstrapError, ConfigError, EventPromptError,
    GameMasterError, GenerationError, PrivateInspirationError, RepositoryError,
    TransientPostgresFailure, classify_postgres_sqlstate,
};
pub use generation_ledger::{
    GenerationLedgerError, InlineGenerationAttempt, InlineGenerationLedger, InlineGenerationRequest,
};
pub use inspiration::{
    CampaignInspirationSettingsProjection, CampaignInspirationStatus,
    DisableCampaignInspirationCommand, OpaqueInspirationId, SetCampaignInspirationPauseCommand,
};
pub use repository::{
    CAMPAIGN_EXPORT_SCHEMA_VERSION, CAMPAIGN_HISTORY_DEFAULT_LIMIT, CAMPAIGN_HISTORY_MAX_LIMIT,
    CAMPAIGN_LIFECYCLE_SCHEMA_VERSION, CampaignLifecycleCommand, CampaignLifecycleOutcome,
    CampaignLifecycleState, CampaignPlaySession, CampaignPrivateExportV1, CampaignPrivateRecap,
    CampaignSummary, CampaignTurnHistoryItem, CampaignTurnHistoryPage, CompleteRecoveryManifest,
    DATABASE_OPERATIONS_SNAPSHOT_SCHEMA_VERSION, DATABASE_RECOVERY_MANIFEST_SCHEMA_VERSION,
    DatabaseOperationsSnapshot, DatabaseRecoveryManifest, DeleteCampaignCommand,
    EndPlaySessionCommand, GeneratePrivateRecapCommand, GenerationBudgetDenialCount,
    GenerationQueueStateCount, OperationalOutcomeCount, PRIVATE_RECAP_SCHEMA_VERSION,
    PreparedCampaignDeletion, RecoveryArtifactFileEntry, RecoveryCampaignManifestEntry,
    RecoveryManifestError, RecoveryMigrationManifestEntry, RestoreCampaignExportCommand,
    StartPlaySessionCommand, VerifiedRecoveryFile,
};
pub use repository::{
    CampaignMembershipRow, CreateCampaignWithOwnerOutcome, MembershipCampaignSummary,
    MembershipRole, MembershipState,
};
#[cfg(feature = "legacy-import")]
pub use repository::{
    LEGACY_IMPORT_SCHEMA_VERSION, LegacyImportCounts, LegacyImportError, LegacyImportReport,
    import_legacy_sqlite,
};
pub use scene_images::{
    DeliveredSceneImage, ImageBrief, SceneImageCleanupOutcome, SceneImageEnqueueOutcome,
    SceneImageError, SceneImageService, SceneImageServiceStatus, SceneImageWorkerOutcome,
};
pub use seed::{CampaignSeed, SeedVault, SeedVaultError};
