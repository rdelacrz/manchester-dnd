use std::sync::Arc;

use crate::{
    application::GameApplicationService,
    campaign_pins::CampaignPinRuntime,
    config::{AppConfig, ContentPackConfig},
    content::{ActiveContentCatalog, load_bundled_content_catalog},
    error::{ApplicationError, BootstrapError, ConfigError},
    generation::{GenerationProviders, ImageGenerator, TextGenerator},
    generation_ledger::InlineGenerationLedger,
    gm::GameMasterService,
    inspiration::PrivateInspirationApplicationService,
    repository::PostgresRepository,
    scene_images::SceneImageService,
    seed::SeedVault,
    typed_gm::{TypedGmService, TypedGmServiceConfig},
};

/// Cloneable dependencies intended to be placed directly in Axum state.
#[derive(Clone)]
pub struct ServerContext {
    pub config: Arc<AppConfig>,
    pub active_content: Arc<ActiveContentCatalog>,
    pub application: GameApplicationService,
    pub authentication: crate::auth::AuthService,
    pub text_generator: Arc<dyn TextGenerator>,
    pub image_generator: Arc<dyn ImageGenerator>,
    pub game_master: GameMasterService,
    pub typed_game_master: TypedGmService,
    pub generation_ledger: InlineGenerationLedger,
    pub scene_images: SceneImageService,
    pub private_inspiration: PrivateInspirationApplicationService,
    pub seed_vault: Arc<SeedVault>,
}

impl ServerContext {
    pub async fn bootstrap() -> Result<Self, BootstrapError> {
        Self::from_config(AppConfig::load()?).await
    }

    pub async fn from_config(config: AppConfig) -> Result<Self, BootstrapError> {
        config.validate_access_mode()?;
        // Content is validated before opening the database or creating secrets:
        // an absent, quarantined, or unpinned required pack aborts boot cleanly.
        let active_content = load_active_content(&config.content_packs)?;
        let repository =
            PostgresRepository::connect(config.database_url.expose_secret(), config.database)
                .await?;
        let authentication =
            crate::auth::AuthService::new(repository.clone(), config.authentication.clone())
                .map_err(|_| {
                    BootstrapError::Config(ConfigError::InvalidValue {
                        name: "AUTH_ARGON2_*",
                        reason: "Argon2id parameter construction failed".to_owned(),
                    })
                })?;
        let generation_ledger = InlineGenerationLedger::new(
            repository.clone(),
            &config.text_llm,
            &config.generation_governance,
        );
        let campaign_pins = Arc::new(CampaignPinRuntime::from_catalog(&active_content));
        let seed_vault = Arc::new(SeedVault::load_or_create(&config.rng_master_key_file)?);
        let providers = GenerationProviders::from_profiles(&config.text_llm, &config.image_llm)?;
        let game_master = GameMasterService::new(providers.text.clone());
        let typed_game_master = TypedGmService::new(
            providers.text.clone(),
            TypedGmServiceConfig::private_mvp(config.text_llm.non_secret_fingerprint("typed-text")),
        )
        .expect("the compiled private-MVP typed GM limits are valid");
        let application = GameApplicationService::new(
            config.access_mode,
            repository.clone(),
            seed_vault.clone(),
            campaign_pins,
        );
        let private_inspiration = PrivateInspirationApplicationService::new(
            repository.clone(),
            config.inspiration_enabled,
            seed_vault.clone(),
        );
        let scene_images = SceneImageService::new(
            repository,
            providers.image.clone(),
            &config.image_llm,
            &config.generation_governance,
            &config.image_artifact_root,
        )?;

        Ok(Self {
            config: Arc::new(config),
            active_content,
            application,
            authentication,
            text_generator: providers.text,
            image_generator: providers.image,
            game_master,
            typed_game_master,
            generation_ledger,
            scene_images,
            private_inspiration,
            seed_vault,
        })
    }

    pub async fn health_check(&self) -> Result<(), ApplicationError> {
        self.application.health_check().await
    }
}

fn load_active_content(
    config: &ContentPackConfig,
) -> Result<Arc<ActiveContentCatalog>, BootstrapError> {
    Ok(Arc::new(load_bundled_content_catalog(
        &config.root,
        &config.default_theme_pack_id,
    )?))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use manchester_dnd_core::hero::{CORE_CONTENT_PACK_ID, RAINBOUND_THEME_PACK_ID};

    use super::*;
    use crate::content::ContentCatalogError;

    #[test]
    fn context_loads_the_exact_immutable_catalog_before_external_dependencies() {
        let config = ContentPackConfig {
            root: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/packs"),
            default_theme_pack_id: RAINBOUND_THEME_PACK_ID.to_owned(),
        };
        let catalog = load_active_content(&config).unwrap();
        assert_eq!(catalog.packs().len(), 3);
        assert!(catalog.pack(CORE_CONTENT_PACK_ID).is_some());
        assert_eq!(
            catalog.default_theme().identity().id,
            RAINBOUND_THEME_PACK_ID
        );
    }

    #[test]
    fn context_fails_closed_before_boot_when_required_content_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let config = ContentPackConfig {
            root: root.path().to_owned(),
            default_theme_pack_id: RAINBOUND_THEME_PACK_ID.to_owned(),
        };
        assert!(matches!(
            load_active_content(&config),
            Err(BootstrapError::Content(
                ContentCatalogError::RequiredPackMissing {
                    pack_id: CORE_CONTENT_PACK_ID
                }
            ))
        ));
    }
}
