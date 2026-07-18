use leptos::{prelude::*, server_fn::codec::Json, task::spawn_local};
use manchester_dnd_core::LocalCampaignViewDto;
use serde::{Deserialize, Serialize};

use crate::{campaign::PublicGameError, freeform::FreeformIntentState};

pub const INSPIRATION_CONTROL_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspirationControlAction {
    Pause,
    Resume,
    Disable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspirationControlCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_settings_revision: u64,
    pub idempotency_key: String,
    pub action: InspirationControlAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspirationStatusView {
    pub schema_version: u16,
    pub deployment_enabled: bool,
    pub global_generation_disabled: bool,
    pub global_control_revision: u64,
    pub configured: bool,
    pub settings_revision: Option<u64>,
    pub enabled: bool,
    pub paused: bool,
    pub safety_setup_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum InspirationStatusResponse {
    Ready(InspirationStatusView),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspirationInterventionAction {
    Veil,
    VetoSource,
    VetoCategory,
    Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspirationInterventionCommand {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub idempotency_key: String,
    pub presentation_id: String,
    pub action: InspirationInterventionAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspirationInterventionView {
    pub presentation_id: String,
    pub action: InspirationInterventionAction,
    pub presentation_hidden: bool,
    pub settings_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum InspirationInterventionResponse {
    Applied(InspirationInterventionView),
    Rejected(PublicGameError),
}

#[server(input = Json)]
pub async fn load_inspiration_status(
    campaign_session_id: String,
) -> Result<InspirationStatusResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::{OpaqueInspirationId, ServerContext};

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(InspirationStatusResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        if !manchester_dnd_core::is_valid_opaque_id(&campaign_session_id) {
            return Ok(InspirationStatusResponse::Rejected(public_control_error(
                "invalid_inspiration_command",
                "That inspiration control is invalid.",
                false,
                None,
                correlation_id,
            )));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(InspirationStatusResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let campaign_id = match OpaqueInspirationId::new(campaign_session_id) {
            Ok(value) => value,
            Err(_) => {
                return Ok(InspirationStatusResponse::Rejected(public_control_error(
                    "invalid_inspiration_command",
                    "That inspiration control is invalid.",
                    false,
                    None,
                    correlation_id,
                )));
            }
        };
        match context
            .private_inspiration
            .campaign_status(&campaign_id)
            .await
        {
            Ok(status) => Ok(InspirationStatusResponse::Ready(status_view(status))),
            Err(error) => Ok(InspirationStatusResponse::Rejected(private_error(
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

#[server(input = Json)]
pub async fn apply_inspiration_control(
    command: InspirationControlCommand,
) -> Result<InspirationStatusResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::{
            DisableCampaignInspirationCommand, OpaqueInspirationId, ServerContext,
            SetCampaignInspirationPauseCommand,
        };

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(InspirationStatusResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        if command.schema_version != INSPIRATION_CONTROL_SCHEMA_VERSION
            || !manchester_dnd_core::is_valid_opaque_id(&command.campaign_session_id)
            || !manchester_dnd_core::is_valid_opaque_id(&command.idempotency_key)
            || command.expected_settings_revision == 0
        {
            return Ok(InspirationStatusResponse::Rejected(public_control_error(
                "invalid_inspiration_command",
                "That inspiration control is invalid.",
                false,
                None,
                correlation_id,
            )));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(InspirationStatusResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let campaign_session_id = match OpaqueInspirationId::new(command.campaign_session_id) {
            Ok(value) => value,
            Err(_) => {
                return Ok(InspirationStatusResponse::Rejected(public_control_error(
                    "invalid_inspiration_command",
                    "That inspiration control is invalid.",
                    false,
                    None,
                    correlation_id,
                )));
            }
        };
        let idempotency_key = match OpaqueInspirationId::new(command.idempotency_key) {
            Ok(value) => value,
            Err(_) => {
                return Ok(InspirationStatusResponse::Rejected(public_control_error(
                    "invalid_inspiration_command",
                    "That inspiration control is invalid.",
                    false,
                    None,
                    correlation_id,
                )));
            }
        };
        let result = match command.action {
            InspirationControlAction::Pause | InspirationControlAction::Resume => {
                context
                    .private_inspiration
                    .set_campaign_pause(SetCampaignInspirationPauseCommand {
                        schema_version: 1,
                        campaign_session_id,
                        idempotency_key,
                        expected_revision: command.expected_settings_revision,
                        paused: command.action == InspirationControlAction::Pause,
                    })
                    .await
            }
            InspirationControlAction::Disable => {
                context
                    .private_inspiration
                    .disable_campaign(DisableCampaignInspirationCommand {
                        schema_version: 1,
                        campaign_session_id,
                        idempotency_key,
                        expected_revision: command.expected_settings_revision,
                    })
                    .await
            }
        };
        match result {
            Ok(settings) => match context
                .private_inspiration
                .campaign_status(&settings.campaign_session_id)
                .await
            {
                Ok(status) => Ok(InspirationStatusResponse::Ready(status_view(status))),
                Err(error) => Ok(InspirationStatusResponse::Rejected(private_error(
                    &error,
                    correlation_id,
                ))),
            },
            Err(error) => Ok(InspirationStatusResponse::Rejected(private_error(
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

#[server(input = Json)]
pub async fn apply_inspiration_intervention(
    command: InspirationInterventionCommand,
) -> Result<InspirationInterventionResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::{
            ServerContext,
            inspiration::{
                ApplyPresentationPrivacyCommand, OpaqueInspirationId,
                PRIVATE_INSPIRATION_SCHEMA_VERSION, PresentationPrivacyAction,
            },
        };

        let headers = crate::campaign::request_headers().await;
        let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
        {
            return Ok(InspirationInterventionResponse::Rejected(
                crate::campaign::invalid_origin_error(correlation_id),
            ));
        }
        if command.schema_version != INSPIRATION_CONTROL_SCHEMA_VERSION
            || !manchester_dnd_core::is_valid_opaque_id(&command.campaign_session_id)
            || !manchester_dnd_core::is_valid_opaque_id(&command.idempotency_key)
            || !manchester_dnd_core::is_valid_opaque_id(&command.presentation_id)
        {
            return Ok(InspirationInterventionResponse::Rejected(
                public_control_error(
                    "invalid_inspiration_intervention",
                    "That privacy intervention is invalid.",
                    false,
                    None,
                    correlation_id,
                ),
            ));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(InspirationInterventionResponse::Rejected(
                crate::campaign::internal_error(correlation_id),
            ));
        };
        let parsed = (
            OpaqueInspirationId::new(command.campaign_session_id),
            OpaqueInspirationId::new(command.idempotency_key),
            OpaqueInspirationId::new(command.presentation_id),
        );
        let (Ok(campaign_session_id), Ok(idempotency_key), Ok(presentation_id)) = parsed else {
            return Ok(InspirationInterventionResponse::Rejected(
                public_control_error(
                    "invalid_inspiration_intervention",
                    "That privacy intervention is invalid.",
                    false,
                    None,
                    correlation_id,
                ),
            ));
        };
        let action = match command.action {
            InspirationInterventionAction::Veil => PresentationPrivacyAction::Veil,
            InspirationInterventionAction::VetoSource => PresentationPrivacyAction::VetoSource,
            InspirationInterventionAction::VetoCategory => PresentationPrivacyAction::VetoCategory,
            InspirationInterventionAction::Report => PresentationPrivacyAction::Report,
        };
        match context
            .private_inspiration
            .apply_presentation_privacy_control(ApplyPresentationPrivacyCommand {
                schema_version: PRIVATE_INSPIRATION_SCHEMA_VERSION,
                campaign_session_id,
                idempotency_key,
                presentation_id,
                action,
            })
            .await
        {
            Ok(outcome) => Ok(InspirationInterventionResponse::Applied(
                InspirationInterventionView {
                    presentation_id: outcome.presentation_id.to_string(),
                    action: command.action,
                    presentation_hidden: outcome.presentation_hidden,
                    settings_revision: outcome.settings_revision,
                },
            )),
            Err(error) => Ok(InspirationInterventionResponse::Rejected(private_error(
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
fn status_view(status: manchester_dnd_server::CampaignInspirationStatus) -> InspirationStatusView {
    let settings = status.settings;
    InspirationStatusView {
        schema_version: INSPIRATION_CONTROL_SCHEMA_VERSION,
        deployment_enabled: status.deployment_enabled,
        global_generation_disabled: status.global_generation_disabled,
        global_control_revision: status.global_control_revision,
        configured: settings.is_some(),
        settings_revision: settings.as_ref().map(|value| value.revision),
        enabled: settings.as_ref().is_some_and(|value| value.enabled),
        paused: settings
            .as_ref()
            .is_some_and(|value| value.generation_paused),
        safety_setup_complete: settings
            .as_ref()
            .is_some_and(|value| value.safety_setup_complete),
    }
}

#[cfg(feature = "ssr")]
fn private_error(
    error: &manchester_dnd_server::PrivateInspirationError,
    correlation_id: String,
) -> PublicGameError {
    tracing::warn!(
        correlation_id,
        code = error.public_code(),
        "private inspiration control rejected"
    );
    public_control_error(
        error.public_code(),
        error.safe_message(),
        error.retryable(),
        error.current_revision(),
        correlation_id,
    )
}

#[cfg(feature = "ssr")]
fn public_control_error(
    code: &str,
    message: &str,
    retryable: bool,
    current_revision: Option<u64>,
    correlation_id: String,
) -> PublicGameError {
    PublicGameError {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable,
        current_revision,
        correlation_id,
        alternatives: Vec::new(),
    }
}

fn load_status_into(
    campaign_id: String,
    status: RwSignal<Option<InspirationStatusView>>,
    pending: RwSignal<bool>,
    notice: RwSignal<String>,
) {
    pending.set(true);
    spawn_local(async move {
        match load_inspiration_status(campaign_id).await {
            Ok(InspirationStatusResponse::Ready(value)) => {
                notice.set(status_description(&value).to_owned());
                status.set(Some(value));
            }
            Ok(InspirationStatusResponse::Rejected(error)) => {
                notice.set(format!("{} [{}]", error.message, error.code));
                status.set(None);
            }
            Err(_) => {
                notice.set("The private-inspiration status could not be loaded.".to_owned());
                status.set(None);
            }
        }
        pending.set(false);
    });
}

#[component]
pub fn PrivacyControls(
    campaign_view: RwSignal<Option<LocalCampaignViewDto>>,
    freeform_intent_state: FreeformIntentState,
) -> impl IntoView {
    let status = RwSignal::new(None::<InspirationStatusView>);
    let pending = RwSignal::new(false);
    let notice = RwSignal::new("Loading private-inspiration controls…".to_owned());

    Effect::new(move |_| {
        if let Some(campaign) = campaign_view.get() {
            load_status_into(campaign.campaign_session_id, status, pending, notice);
        }
    });

    let apply = move |action: InspirationControlAction| {
        let Some(campaign) = campaign_view.get_untracked() else {
            notice.set("Load a campaign before changing its safety controls.".to_owned());
            return;
        };
        let Some(current) = status.get_untracked() else {
            notice.set("Private inspiration is not configured for this campaign.".to_owned());
            return;
        };
        let Some(expected_settings_revision) = current.settings_revision else {
            notice.set("Private inspiration is not configured for this campaign.".to_owned());
            return;
        };
        pending.set(true);
        notice.set(
            match action {
                InspirationControlAction::Pause => "Pausing private generation…",
                InspirationControlAction::Resume => "Resuming private generation…",
                InspirationControlAction::Disable => "Disabling campaign inspiration…",
            }
            .to_owned(),
        );
        spawn_local(async move {
            let campaign_session_id = campaign.campaign_session_id;
            let command = InspirationControlCommand {
                schema_version: INSPIRATION_CONTROL_SCHEMA_VERSION,
                campaign_session_id: campaign_session_id.clone(),
                expected_settings_revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                action,
            };
            match apply_inspiration_control(command).await {
                Ok(InspirationStatusResponse::Ready(value)) => {
                    notice.set(status_description(&value).to_owned());
                    status.set(Some(value));
                }
                Ok(InspirationStatusResponse::Rejected(error)) => {
                    notice.set(format!("{} [{}]", error.message, error.code));
                    if error.code == "inspiration_revision_conflict" {
                        load_status_into(campaign_session_id, status, pending, notice);
                        return;
                    }
                }
                Err(_) => notice.set(
                    "The request was interrupted. Reload status before trying again.".to_owned(),
                ),
            }
            pending.set(false);
        });
    };

    let intervene = move |action: InspirationInterventionAction| {
        let Some(campaign) = campaign_view.get_untracked() else {
            notice.set("Load a campaign before using a privacy intervention.".to_owned());
            return;
        };
        let Some(presentation_id) = freeform_intent_state.current_private_presentation_id() else {
            notice.set(
                "No currently displayed private-inspiration passage needs intervention.".to_owned(),
            );
            return;
        };
        pending.set(true);
        notice.set(
            match action {
                InspirationInterventionAction::Veil => "Veiling the current passage…",
                InspirationInterventionAction::VetoSource => {
                    "Vetoing this source and restoring unrelated narration…"
                }
                InspirationInterventionAction::VetoCategory => {
                    "Disabling this category and restoring unrelated narration…"
                }
                InspirationInterventionAction::Report => {
                    "Hiding the passage, pausing generation, and recording a private report…"
                }
            }
            .to_owned(),
        );
        spawn_local(async move {
            let campaign_session_id = campaign.campaign_session_id;
            match apply_inspiration_intervention(InspirationInterventionCommand {
                schema_version: INSPIRATION_CONTROL_SCHEMA_VERSION,
                campaign_session_id: campaign_session_id.clone(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                presentation_id: presentation_id.clone(),
                action,
            })
            .await
            {
                Ok(InspirationInterventionResponse::Applied(outcome))
                    if outcome.presentation_id == presentation_id
                        && outcome.presentation_hidden =>
                {
                    let use_engine_fallback = matches!(
                        action,
                        InspirationInterventionAction::VetoSource
                            | InspirationInterventionAction::VetoCategory
                    );
                    freeform_intent_state
                        .hide_private_presentation(&presentation_id, use_engine_fallback);
                    notice.set(
                        match action {
                            InspirationInterventionAction::Veil => {
                                "The current passage is veiled. Saved mechanics are unchanged."
                            }
                            InspirationInterventionAction::VetoSource => {
                                "The source is vetoed and unrelated deterministic narration is shown."
                            }
                            InspirationInterventionAction::VetoCategory => {
                                "The category is disabled and unrelated deterministic narration is shown."
                            }
                            InspirationInterventionAction::Report => {
                                "The passage is hidden, private generation is paused, and an opaque report is recorded."
                            }
                        }
                        .to_owned(),
                    );
                    if outcome.settings_revision.is_some() {
                        load_status_into(campaign_session_id, status, pending, notice);
                        return;
                    }
                }
                Ok(InspirationInterventionResponse::Applied(_)) => {
                    notice.set("The server returned mismatched intervention evidence.".to_owned());
                }
                Ok(InspirationInterventionResponse::Rejected(error)) => {
                    notice.set(format!("{} [{}]", error.message, error.code));
                }
                Err(_) => notice.set(
                    "The intervention was interrupted. Reload before trying a new request."
                        .to_owned(),
                ),
            }
            pending.set(false);
        });
    };

    view! {
        <article class="panel privacy-panel" id="privacy" aria-labelledby="privacy-heading">
            <p class="eyebrow">"MEMORY, WITH BOUNDARIES"</p>
            <h2 id="privacy-heading">"Real stories stay under your control."</h2>
            <p>
                "Private inspiration defaults off. Pausing prevents a draw without changing reviewed consent. Disabling revokes active campaign grants and cancels pending derived work."
            </p>
            <div class="privacy-tags" aria-label="Private inspiration safeguards">
                <span>"Consent gates"</span><span>"Audited cooldowns"</span><span>"Private by default"</span>
            </div>
            <div class="privacy-controls" role="group" aria-label="Private inspiration controls">
                <button
                    type="button"
                    disabled=move || {
                        pending.get()
                            || status.get().is_none_or(|value| !value.enabled || value.paused)
                    }
                    on:click=move |_| apply(InspirationControlAction::Pause)
                >
                    "Pause private generation"
                </button>
                <button
                    type="button"
                    disabled=move || {
                        pending.get()
                            || status.get().is_none_or(|value| {
                                !value.deployment_enabled || !value.enabled || !value.paused
                                    || value.global_generation_disabled
                            })
                    }
                    on:click=move |_| apply(InspirationControlAction::Resume)
                >
                    "Resume private generation"
                </button>
                <button
                    type="button"
                    class="danger-button"
                    disabled=move || {
                        pending.get() || status.get().is_none_or(|value| !value.enabled)
                    }
                    on:click=move |_| apply(InspirationControlAction::Disable)
                >
                    "Disable all inspiration"
                </button>
            </div>
            <h3>"Current passage controls"</h3>
            <p>"These actions never ask for a reason and never change a saved roll or encounter result."</p>
            <div class="privacy-controls privacy-interventions" role="group" aria-label="Current private passage interventions">
                <button
                    type="button"
                    disabled=move || {
                        pending.get()
                            || freeform_intent_state.current_private_presentation_id().is_none()
                    }
                    on:click=move |_| intervene(InspirationInterventionAction::Veil)
                >
                    "Veil current passage"
                </button>
                <button
                    type="button"
                    class="danger-button"
                    disabled=move || {
                        pending.get()
                            || freeform_intent_state.current_private_presentation_id().is_none()
                    }
                    on:click=move |_| intervene(InspirationInterventionAction::VetoSource)
                >
                    "Veto this source"
                </button>
                <button
                    type="button"
                    class="danger-button"
                    disabled=move || {
                        pending.get()
                            || freeform_intent_state.current_private_presentation_id().is_none()
                    }
                    on:click=move |_| intervene(InspirationInterventionAction::VetoCategory)
                >
                    "Disable this category"
                </button>
                <button
                    type="button"
                    class="danger-button"
                    disabled=move || {
                        pending.get()
                            || freeform_intent_state.current_private_presentation_id().is_none()
                    }
                    on:click=move |_| intervene(InspirationInterventionAction::Report)
                >
                    "Report a privacy issue"
                </button>
            </div>
            <p class="privacy-status" role="status" aria-live="polite" aria-busy=move || pending.get()>
                {move || notice.get()}
            </p>
        </article>
    }
}

fn status_description(status: &InspirationStatusView) -> &'static str {
    if !status.deployment_enabled {
        "Private inspiration is disabled for this installation."
    } else if status.global_generation_disabled {
        "Private inspiration is quarantined by the global incident switch."
    } else if !status.configured {
        "Private inspiration has not been configured for this campaign."
    } else if !status.enabled {
        "Private inspiration is disabled for this campaign."
    } else if status.paused {
        "Private generation is paused; deterministic play remains available."
    } else if !status.safety_setup_complete {
        "Private inspiration is blocked until campaign safety setup is complete."
    } else {
        "Private inspiration is enabled behind current consent and safety gates."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_copy_never_implies_disabled_or_unconfigured_sources_are_available() {
        let mut status = InspirationStatusView {
            schema_version: 1,
            deployment_enabled: false,
            global_generation_disabled: false,
            global_control_revision: 1,
            configured: false,
            settings_revision: None,
            enabled: false,
            paused: false,
            safety_setup_complete: false,
        };
        assert!(status_description(&status).contains("installation"));
        status.deployment_enabled = true;
        assert!(status_description(&status).contains("not been configured"));
        status.global_generation_disabled = true;
        assert!(status_description(&status).contains("incident switch"));
        status.global_generation_disabled = false;
        status.configured = true;
        status.settings_revision = Some(1);
        assert!(status_description(&status).contains("disabled"));
        status.enabled = true;
        status.paused = true;
        assert!(status_description(&status).contains("paused"));
    }
}
