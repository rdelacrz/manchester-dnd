use std::sync::Arc;

use crate::{
    config::AppConfig,
    error::BootstrapError,
    events::{EventPrompt, EventPromptLoader},
    generation::{GenerationProviders, ImageGenerator, TextGenerator},
    gm::GameMasterService,
    repository::SqliteRepository,
};

/// Cloneable dependencies intended to be placed directly in Axum state.
#[derive(Clone)]
pub struct ServerContext {
    pub config: Arc<AppConfig>,
    pub repository: SqliteRepository,
    pub text_generator: Arc<dyn TextGenerator>,
    pub image_generator: Arc<dyn ImageGenerator>,
    pub game_master: GameMasterService,
    pub event_prompts: Arc<Vec<EventPrompt>>,
}

impl ServerContext {
    pub async fn bootstrap() -> Result<Self, BootstrapError> {
        Self::from_config(AppConfig::load()?).await
    }

    pub async fn from_config(config: AppConfig) -> Result<Self, BootstrapError> {
        let repository = SqliteRepository::connect(&config.database_url).await?;
        let providers = GenerationProviders::from_profiles(&config.text_llm, &config.image_llm)?;
        let event_prompts = EventPromptLoader.load_dir(&config.event_prompts_dir)?;
        let game_master = GameMasterService::new(providers.text.clone());

        Ok(Self {
            config: Arc::new(config),
            repository,
            text_generator: providers.text,
            image_generator: providers.image,
            game_master,
            event_prompts: Arc::new(event_prompts),
        })
    }
}
