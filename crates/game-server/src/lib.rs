//! Server-side boundaries for configuration, generation, persistence, event
//! prompts, and AI game-master orchestration.
//!
//! The rules engine remains in `manchester-dnd-core`. In particular, the GM
//! service in this crate only returns proposals; callers must validate and
//! apply them through the authoritative core domain.

pub mod config;
pub mod context;
pub mod error;
pub mod events;
pub mod generation;
pub mod gm;
pub mod repository;

pub use config::{AppConfig, LlmBackend, LlmProfile, SecretString};
pub use context::ServerContext;
pub use error::{
    BootstrapError, ConfigError, EventPromptError, GameMasterError, GenerationError,
    RepositoryError,
};
