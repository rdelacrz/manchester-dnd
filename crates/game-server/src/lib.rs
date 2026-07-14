//! Server-side boundaries for configuration, generation, persistence, event
//! prompts, and AI game-master orchestration.
//!
//! The rules engine remains in `manchester-dnd-core`. In particular, the GM
//! service in this crate only returns proposals; callers must validate and
//! apply them through the authoritative core domain.

pub mod application;
pub mod config;
pub mod context;
pub mod error;
pub mod events;
pub mod generation;
pub mod gm;
pub mod repository;

pub use application::{
    GameApplicationService, LOCAL_CAMPAIGN_SESSION_ID, LOCAL_CHARACTER_ID,
    LOCAL_EXPLORATION_ACTION_ID, UnixTimeSource,
};
pub use config::{AccessMode, AppConfig, LlmBackend, LlmProfile, SecretString};
pub use context::ServerContext;
pub use error::{
    ApplicationError, BootstrapError, ConfigError, EventPromptError, GameMasterError,
    GenerationError, RepositoryError,
};
