use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use manchester_dnd_core::LocalCampaignViewDto;
use serde::{Deserialize, Serialize};

use crate::campaign::PublicGameError;

const SCENE_IMAGE_VIEW_SCHEMA_VERSION: u16 = 1;
const IMAGE_ROLLING_LIMIT: u64 = 3;
const IMAGE_LIFETIME_LIMIT: u64 = 10;
const IMAGE_TURN_LIMIT: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSceneImageCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub replacement: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelSceneImageCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneImageState {
    NoRequest,
    Queued,
    Running,
    RetryScheduled,
    Ready,
    Rejected,
    Unavailable,
    Cancelled,
}

impl SceneImageState {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::RetryScheduled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneImageView {
    pub schema_version: u16,
    pub provider_enabled: bool,
    pub provider_temporarily_unavailable: bool,
    pub state: SceneImageState,
    pub job_id: Option<String>,
    pub attempt_count: u8,
    pub max_attempts: u8,
    pub artifact_url: Option<String>,
    pub alt_text: Option<String>,
    pub rolling_requests_used: u64,
    pub lifetime_requests_used: u64,
    pub turn_requests_used: u64,
    pub estimated_request_cost_microusd: u64,
    pub campaign_cost_used_microusd: u64,
    pub campaign_cost_limit_microusd: u64,
}

impl SceneImageView {
    fn request_available(&self) -> bool {
        let cost_available = self.estimated_request_cost_microusd == 0
            || self
                .campaign_cost_used_microusd
                .checked_add(self.estimated_request_cost_microusd)
                .is_some_and(|total| total <= self.campaign_cost_limit_microusd);
        self.provider_enabled
            && !self.provider_temporarily_unavailable
            && !self.state.active()
            && self.rolling_requests_used < IMAGE_ROLLING_LIMIT
            && self.lifetime_requests_used < IMAGE_LIFETIME_LIMIT
            && self.turn_requests_used < IMAGE_TURN_LIMIT
            && cost_available
    }

    fn replacement(&self) -> bool {
        self.turn_requests_used == 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum SceneImageStatusResponse {
    Ready(SceneImageView),
    Rejected(PublicGameError),
}

#[server]
pub async fn load_scene_image_status(
    campaign_session_id: String,
) -> Result<SceneImageStatusResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(SceneImageStatusResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(SceneImageStatusResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        match context.scene_images.status(&campaign_session_id).await {
            Ok(status) => Ok(SceneImageStatusResponse::Ready(map_status(status))),
            Err(error) => Ok(SceneImageStatusResponse::Rejected(image_public_error(
                &error,
                correlation_id,
            ))),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = campaign_session_id;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server]
pub async fn request_scene_image(
    command: RequestSceneImageCommand,
) -> Result<SceneImageStatusResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(SceneImageStatusResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(SceneImageStatusResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        if command.schema_version != SCENE_IMAGE_VIEW_SCHEMA_VERSION {
            return Ok(SceneImageStatusResponse::Rejected(image_command_error(
                correlation_id,
            )));
        }
        match context
            .scene_images
            .request(
                &command.campaign_session_id,
                command.expected_revision,
                &command.idempotency_key,
                command.replacement,
                Some(&correlation_id),
            )
            .await
        {
            Ok(_) => match context
                .scene_images
                .status(&command.campaign_session_id)
                .await
            {
                Ok(status) => Ok(SceneImageStatusResponse::Ready(map_status(status))),
                Err(error) => Ok(SceneImageStatusResponse::Rejected(image_public_error(
                    &error,
                    correlation_id,
                ))),
            },
            Err(error) => Ok(SceneImageStatusResponse::Rejected(image_public_error(
                &error,
                correlation_id,
            ))),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server]
pub async fn cancel_scene_image(
    command: CancelSceneImageCommand,
) -> Result<SceneImageStatusResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(SceneImageStatusResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(SceneImageStatusResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        if command.schema_version != SCENE_IMAGE_VIEW_SCHEMA_VERSION {
            return Ok(SceneImageStatusResponse::Rejected(image_command_error(
                correlation_id,
            )));
        }
        match context
            .scene_images
            .cancel(&command.campaign_session_id, &command.job_id)
            .await
        {
            Ok(_) => match context
                .scene_images
                .status(&command.campaign_session_id)
                .await
            {
                Ok(status) => Ok(SceneImageStatusResponse::Ready(map_status(status))),
                Err(error) => Ok(SceneImageStatusResponse::Rejected(image_public_error(
                    &error,
                    correlation_id,
                ))),
            },
            Err(error) => Ok(SceneImageStatusResponse::Rejected(image_public_error(
                &error,
                correlation_id,
            ))),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(feature = "ssr")]
fn map_status(status: manchester_dnd_server::SceneImageServiceStatus) -> SceneImageView {
    use manchester_dnd_server::repository::jobs::{GenerationFailureCode, GenerationJobState};

    let state = status.latest_job.as_ref().map_or_else(
        || {
            if status.provider_enabled {
                SceneImageState::NoRequest
            } else {
                SceneImageState::Unavailable
            }
        },
        |job| match job.state {
            GenerationJobState::Queued if job.attempt_count == 0 => SceneImageState::Queued,
            GenerationJobState::Queued => SceneImageState::RetryScheduled,
            GenerationJobState::Running => SceneImageState::Running,
            GenerationJobState::Succeeded if status.artifact.is_some() => SceneImageState::Ready,
            GenerationJobState::Succeeded => SceneImageState::Unavailable,
            GenerationJobState::Failed
                if matches!(
                    job.last_failure_code,
                    Some(
                        GenerationFailureCode::ProviderRejected
                            | GenerationFailureCode::MalformedResponse
                            | GenerationFailureCode::UnsafeOutput
                            | GenerationFailureCode::InvalidArtifact
                    )
                ) =>
            {
                SceneImageState::Rejected
            }
            GenerationJobState::Failed => SceneImageState::Unavailable,
            GenerationJobState::Cancelled => SceneImageState::Cancelled,
        },
    );
    let artifact_url = status
        .artifact
        .as_ref()
        .map(|artifact| format!("/api/local/images/{}/web", artifact.artifact_id));
    SceneImageView {
        schema_version: SCENE_IMAGE_VIEW_SCHEMA_VERSION,
        provider_enabled: status.provider_enabled,
        provider_temporarily_unavailable: status.provider_temporarily_unavailable,
        state,
        job_id: status.latest_job.as_ref().map(|job| job.id.clone()),
        attempt_count: status
            .latest_job
            .as_ref()
            .map_or(0, |job| job.attempt_count),
        max_attempts: status.latest_job.as_ref().map_or(0, |job| job.max_attempts),
        artifact_url,
        alt_text: status
            .artifact
            .as_ref()
            .map(|artifact| artifact.alt_text.clone()),
        rolling_requests_used: status.counts.rolling_day,
        lifetime_requests_used: status.counts.campaign_lifetime,
        turn_requests_used: status.counts.source_turn,
        estimated_request_cost_microusd: status.estimated_cost_microusd,
        campaign_cost_used_microusd: status.campaign_cost_used_microusd,
        campaign_cost_limit_microusd: status.campaign_cost_limit_microusd,
    }
}

#[cfg(feature = "ssr")]
fn image_public_error(
    error: &manchester_dnd_server::SceneImageError,
    correlation_id: String,
) -> PublicGameError {
    use manchester_dnd_server::SceneImageError;

    let (code, message, retryable, current_revision) = match error {
        SceneImageError::Disabled => (
            "image_generation_disabled",
            "Scene images are unavailable in this local profile.",
            false,
            None,
        ),
        SceneImageError::InvalidCommand => (
            "invalid_image_command",
            "That scene-image request is invalid.",
            false,
            None,
        ),
        SceneImageError::WrongCampaign | SceneImageError::NotFound => (
            "image_not_found",
            "That scene image is unavailable.",
            false,
            None,
        ),
        SceneImageError::RevisionConflict { actual, .. } => (
            "revision_conflict",
            "The campaign changed; reload before requesting an image.",
            true,
            Some(*actual),
        ),
        SceneImageError::NoCommittedScene => (
            "image_scene_unavailable",
            "Commit an encounter scene before requesting an illustration.",
            false,
            None,
        ),
        SceneImageError::PolicyRejected => (
            "image_policy_rejected",
            "The scene could not be illustrated within the safety policy.",
            false,
            None,
        ),
        SceneImageError::BudgetExceeded => (
            "image_budget_exceeded",
            "The campaign's scene-image request or cost cap has been reached.",
            false,
            None,
        ),
        SceneImageError::ReplacementLimit => (
            "image_replacement_limit",
            "This scene has already used its one replacement image.",
            false,
            None,
        ),
        SceneImageError::CircuitOpen => (
            "image_provider_unavailable",
            "The image provider is temporarily unavailable; play can continue.",
            true,
            None,
        ),
        SceneImageError::BriefSerialization(_)
        | SceneImageError::Store(_)
        | SceneImageError::Repository(_)
        | SceneImageError::Generation(_)
        | SceneImageError::Storage(_)
        | SceneImageError::InvalidArtifact(_)
        | SceneImageError::Codec(_) => (
            "image_unavailable",
            "The scene image is temporarily unavailable; play can continue.",
            false,
            None,
        ),
    };
    tracing::warn!(correlation_id, code, "scene image command rejected");
    PublicGameError {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable,
        current_revision,
        correlation_id,
        alternatives: Vec::new(),
    }
}

#[cfg(feature = "ssr")]
fn image_command_error(correlation_id: String) -> PublicGameError {
    PublicGameError {
        code: "invalid_image_command".to_owned(),
        message: "That scene-image request is invalid.".to_owned(),
        retryable: false,
        current_revision: None,
        correlation_id,
        alternatives: Vec::new(),
    }
}

#[component]
pub fn SceneImagePanel(campaign_view: RwSignal<Option<LocalCampaignViewDto>>) -> impl IntoView {
    let status = RwSignal::new(None::<SceneImageView>);
    let loading = RwSignal::new(false);
    let notice = RwSignal::new("Scene images never block a turn or save.".to_owned());
    let retry = RwSignal::new(None::<RequestSceneImageCommand>);

    Effect::new(move |_| {
        let campaign = campaign_view.get();
        if let Some(campaign) = campaign {
            refresh_status(campaign.campaign_session_id, status, loading, notice);
        } else {
            status.set(None);
        }
    });

    Effect::new(move |_| {
        if status.get().is_some_and(|view| view.state.active()) {
            let campaign_view = campaign_view;
            set_timeout(
                move || {
                    if let Some(campaign) = campaign_view.get_untracked() {
                        refresh_status(campaign.campaign_session_id, status, loading, notice);
                    }
                },
                Duration::from_secs(1),
            );
        }
    });

    let request = move |_| {
        let Some(campaign) = campaign_view.get_untracked() else {
            notice.set("The campaign is not ready yet.".to_owned());
            return;
        };
        let replacement = status
            .get_untracked()
            .is_some_and(|view| view.replacement());
        let command = retry
            .get_untracked()
            .unwrap_or_else(|| RequestSceneImageCommand {
                schema_version: SCENE_IMAGE_VIEW_SCHEMA_VERSION,
                campaign_session_id: campaign.campaign_session_id,
                expected_revision: campaign.revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                replacement,
            });
        retry.set(Some(command.clone()));
        loading.set(true);
        notice.set("The durable image request is being queued…".to_owned());
        spawn_local(async move {
            match request_scene_image(command).await {
                Ok(SceneImageStatusResponse::Ready(view)) => {
                    status.set(Some(view));
                    retry.set(None);
                    notice.set(
                        "Queued. The encounter remains fully playable while the worker runs."
                            .to_owned(),
                    );
                }
                Ok(SceneImageStatusResponse::Rejected(error)) => {
                    notice.set(format!(
                        "{} [{}; reference {}]",
                        error.message, error.code, error.correlation_id
                    ));
                    if !error.retryable || error.code == "revision_conflict" {
                        retry.set(None);
                    }
                }
                Err(_) => notice.set(
                    "The response was interrupted. Retry reuses the exact image request and cannot enqueue twice."
                        .to_owned(),
                ),
            }
            loading.set(false);
        });
    };

    let cancel = move |_| {
        let Some(view) = status.get_untracked() else {
            return;
        };
        let Some(campaign) = campaign_view.get_untracked() else {
            return;
        };
        let Some(job_id) = view.job_id else {
            return;
        };
        loading.set(true);
        spawn_local(async move {
            match cancel_scene_image(CancelSceneImageCommand {
                schema_version: SCENE_IMAGE_VIEW_SCHEMA_VERSION,
                campaign_session_id: campaign.campaign_session_id,
                job_id,
            })
            .await
            {
                Ok(SceneImageStatusResponse::Ready(view)) => {
                    status.set(Some(view));
                    notice.set("Image work cancelled. The saved turn is unchanged.".to_owned());
                }
                Ok(SceneImageStatusResponse::Rejected(error)) => notice.set(format!(
                    "{} [{}; reference {}]",
                    error.message, error.code, error.correlation_id
                )),
                Err(_) => notice.set(
                    "Cancellation could not be confirmed. Refresh image status before retrying."
                        .to_owned(),
                ),
            }
            loading.set(false);
        });
    };

    view! {
        <article class="panel scene-image-panel" id="scene-image" aria-labelledby="scene-image-heading">
            <div class="panel-heading">
                <div>
                    <p class="eyebrow">"OPTIONAL SCENE ART"</p>
                    <h2 id="scene-image-heading">"Illustrate the committed scene"</h2>
                </div>
                <span class="die-icon" aria-hidden="true">"✦"</span>
            </div>
            <p>
                "The brief uses only committed public fantasy facts. It excludes private inspiration, player text, names, likenesses, hidden state, and provider instructions."
            </p>

            {move || {
                let current = status.get();
                let state = current.as_ref().map_or(SceneImageState::NoRequest, |view| view.state);
                let state_label = scene_image_state_label(state);
                let image = current
                    .as_ref()
                    .and_then(|view| view.artifact_url.clone().zip(view.alt_text.clone()));
                let can_request = current.as_ref().is_some_and(SceneImageView::request_available)
                    && campaign_view.get().is_some_and(|campaign| campaign.encounter.is_some());
                let replacement = current.as_ref().is_some_and(SceneImageView::replacement);
                let active = state.active();
                let usage = current.as_ref().map_or_else(
                    || "Loading request and cost limits…".to_owned(),
                    |view| format!(
                        "Requests: {}/{} in 24h · {}/{} lifetime · {}/{} for this scene. Estimated next cost: {}. Campaign cost: {} / {}.",
                        view.rolling_requests_used,
                        IMAGE_ROLLING_LIMIT,
                        view.lifetime_requests_used,
                        IMAGE_LIFETIME_LIMIT,
                        view.turn_requests_used,
                        IMAGE_TURN_LIMIT,
                        format_microusd(view.estimated_request_cost_microusd),
                        format_microusd(view.campaign_cost_used_microusd),
                        format_microusd(view.campaign_cost_limit_microusd),
                    ),
                );
                view! {
                    <div class="scene-image-frame" data-state=state_label>
                        {if let Some((url, alt)) = image {
                            view! { <img src=url alt=alt/> }.into_any()
                        } else {
                            view! {
                                <div class="scene-image-placeholder" role="img" aria-label="No verified scene image is displayed">
                                    <span aria-hidden="true">"✦"</span>
                                    <p>{state_label}</p>
                                </div>
                            }
                            .into_any()
                        }}
                    </div>
                    <p class="scene-image-state" role="status" aria-live="polite">
                        "Image state: " <strong>{state_label}</strong>
                    </p>
                    <p class="scene-image-budget">{usage}</p>
                    <div class="scene-image-actions">
                        <button
                            class="encounter-action"
                            disabled=move || loading.get() || !can_request
                            on:click=request
                        >
                            {if retry.get().is_some() {
                                "Retry exact image request"
                            } else if replacement {
                                "Request the one replacement"
                            } else {
                                "Request scene image"
                            }}
                        </button>
                        <Show when=move || active>
                            <button
                                class="refresh-button"
                                disabled=move || loading.get()
                                on:click=cancel
                            >
                                "Cancel image work"
                            </button>
                        </Show>
                    </div>
                }
                .into_any()
            }}
            <p class="scene-image-notice" aria-live="polite" aria-busy=move || loading.get()>
                {move || notice.get()}
            </p>
        </article>
    }
}

fn refresh_status(
    campaign_session_id: String,
    status: RwSignal<Option<SceneImageView>>,
    loading: RwSignal<bool>,
    notice: RwSignal<String>,
) {
    loading.set(true);
    spawn_local(async move {
        match load_scene_image_status(campaign_session_id).await {
            Ok(SceneImageStatusResponse::Ready(view)) => status.set(Some(view)),
            Ok(SceneImageStatusResponse::Rejected(error)) => notice.set(format!(
                "{} [{}; reference {}]",
                error.message, error.code, error.correlation_id
            )),
            Err(_) => notice.set(
                "Image status is temporarily unavailable. The saved game is unaffected.".to_owned(),
            ),
        }
        loading.set(false);
    });
}

fn scene_image_state_label(state: SceneImageState) -> &'static str {
    match state {
        SceneImageState::NoRequest => "No image requested",
        SceneImageState::Queued => "Queued",
        SceneImageState::Running => "Generating",
        SceneImageState::RetryScheduled => "Retry scheduled",
        SceneImageState::Ready => "Verified image ready",
        SceneImageState::Rejected => "Rejected by provider or safety checks",
        SceneImageState::Unavailable => "Image generation unavailable",
        SceneImageState::Cancelled => "Cancelled",
    }
}

fn format_microusd(value: u64) -> String {
    format!("${:.6}", value as f64 / 1_000_000.0)
}
