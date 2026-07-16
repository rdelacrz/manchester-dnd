use leptos::{prelude::*, server_fn::codec::Json, task::spawn_local};
use manchester_dnd_core::SessionEventDto;
use serde::{Deserialize, Serialize};

use crate::campaign::PublicGameError;

pub const LIFECYCLE_WIRE_SCHEMA_VERSION: u16 = 1;
const LOCAL_CAMPAIGN_ID: &str = "local-campaign";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStateView {
    Active,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLifecycleView {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub title: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub lifecycle_state: LifecycleStateView,
    pub archived_at: Option<String>,
    pub open_play_session_id: Option<String>,
    pub retention_class: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CampaignListResponse {
    Ready(Vec<CampaignLifecycleView>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleIntent {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub expected_lifecycle_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaySessionIntent {
    pub lifecycle: LifecycleIntent,
    pub play_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteCampaignIntent {
    pub lifecycle: LifecycleIntent,
    pub deletion_id: String,
    pub confirm_permanent_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareDeleteIntent {
    pub schema_version: u16,
    pub expected_lifecycle_revision: u64,
    pub deletion_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleMutationView {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub lifecycle_revision: u64,
    pub lifecycle_state: Option<LifecycleStateView>,
    pub play_session_id: Option<String>,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum LifecycleMutationResponse {
    Committed(LifecycleMutationView),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryItemView {
    pub id: String,
    pub turn_number: u64,
    pub actor_id: Option<String>,
    pub event: SessionEventDto,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryPageView {
    pub schema_version: u16,
    pub items: Vec<HistoryItemView>,
    pub next_after_turn_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum HistoryResponse {
    Ready(HistoryPageView),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    pub schema_version: u16,
    pub after_turn_number: Option<u64>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateExportView {
    pub schema_version: u16,
    pub format: String,
    pub body: String,
    pub canonical_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum PrivateExportResponse {
    Ready(Box<PrivateExportView>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateRecapIntent {
    pub schema_version: u16,
    pub expected_campaign_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateRecapView {
    pub schema_version: u16,
    pub id: String,
    pub campaign_revision: u64,
    pub source_audit_count: u64,
    pub source_audit_digest: String,
    pub template_id: String,
    pub body: String,
    pub body_digest: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum PrivateRecapResponse {
    Ready(Option<Box<PrivateRecapView>>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedDeleteView {
    pub schema_version: u16,
    pub campaign_session_id: String,
    pub deletion_id: String,
    pub campaign_revision: u64,
    pub lifecycle_revision: u64,
    pub canonical_export_digest: String,
    pub canonical_export_json: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum PrepareDeleteResponse {
    Ready(Box<PreparedDeleteView>),
    Rejected(PublicGameError),
}

#[server(input = Json)]
pub async fn list_campaigns() -> Result<CampaignListResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(CampaignListResponse::Rejected(error)),
        };
        match context.application.list_local_campaigns().await {
            Ok(campaigns) => Ok(CampaignListResponse::Ready(
                campaigns.into_iter().map(summary_view).collect(),
            )),
            Err(error) => Ok(CampaignListResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

#[server(input = Json)]
pub async fn create_campaign() -> Result<CampaignListResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(CampaignListResponse::Rejected(error)),
        };
        match context.application.create_local_campaign().await {
            Ok(campaign) => Ok(CampaignListResponse::Ready(vec![summary_view(campaign)])),
            Err(error) => Ok(CampaignListResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

#[server(input = Json)]
pub async fn start_play_session(
    intent: PlaySessionIntent,
) -> Result<LifecycleMutationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::StartPlaySessionCommand;
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(LifecycleMutationResponse::Rejected(error)),
        };
        let command = StartPlaySessionCommand {
            lifecycle: trusted_lifecycle(intent.lifecycle),
            play_session_id: intent.play_session_id,
        };
        match context.application.start_local_play_session(command).await {
            Ok(outcome) => Ok(LifecycleMutationResponse::Committed(outcome_view(outcome))),
            Err(error) => Ok(LifecycleMutationResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = intent;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn end_play_session(
    intent: PlaySessionIntent,
) -> Result<LifecycleMutationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::EndPlaySessionCommand;
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(LifecycleMutationResponse::Rejected(error)),
        };
        let command = EndPlaySessionCommand {
            lifecycle: trusted_lifecycle(intent.lifecycle),
            play_session_id: intent.play_session_id,
        };
        match context.application.end_local_play_session(command).await {
            Ok(outcome) => Ok(LifecycleMutationResponse::Committed(outcome_view(outcome))),
            Err(error) => Ok(LifecycleMutationResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = intent;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn archive_campaign(
    intent: LifecycleIntent,
) -> Result<LifecycleMutationResponse, ServerFnError> {
    mutate_lifecycle(intent, LifecycleAction::Archive).await
}

#[server(input = Json)]
pub async fn restore_archived_campaign(
    intent: LifecycleIntent,
) -> Result<LifecycleMutationResponse, ServerFnError> {
    mutate_lifecycle(intent, LifecycleAction::Restore).await
}

#[server(input = Json)]
pub async fn load_campaign_history(
    request: HistoryRequest,
) -> Result<HistoryResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(HistoryResponse::Rejected(error)),
        };
        if request.schema_version != LIFECYCLE_WIRE_SCHEMA_VERSION {
            return Ok(HistoryResponse::Rejected(invalid_wire_error(
                correlation_id,
            )));
        }
        match context
            .application
            .local_campaign_history(request.after_turn_number, request.limit)
            .await
        {
            Ok(page) => Ok(HistoryResponse::Ready(HistoryPageView {
                schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                items: page
                    .items
                    .into_iter()
                    .map(|item| HistoryItemView {
                        id: item.id,
                        turn_number: item.turn_number,
                        actor_id: item.actor_id,
                        event: item.event,
                        created_at: item.created_at,
                    })
                    .collect(),
                next_after_turn_number: page.next_after_turn_number,
            })),
            Err(error) => Ok(HistoryResponse::Rejected(crate::campaign::public_error(
                &error,
                correlation_id,
            ))),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = request;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn export_canonical_campaign() -> Result<PrivateExportResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(PrivateExportResponse::Rejected(error)),
        };
        match context.application.export_local_campaign_private().await {
            Ok(exported) => match (exported.canonical_json(), exported.canonical_digest()) {
                (Ok(body), Ok(digest)) => {
                    Ok(PrivateExportResponse::Ready(Box::new(PrivateExportView {
                        schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                        format: "application/vnd.manchester-arcana.campaign+json;version=1"
                            .to_owned(),
                        body,
                        canonical_digest: Some(digest.to_string()),
                    })))
                }
                _ => Ok(PrivateExportResponse::Rejected(
                    crate::campaign::internal_error(correlation_id),
                )),
            },
            Err(error) => Ok(PrivateExportResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

#[server(input = Json)]
pub async fn export_readable_campaign() -> Result<PrivateExportResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(PrivateExportResponse::Rejected(error)),
        };
        match context
            .application
            .export_local_campaign_player_readable()
            .await
        {
            Ok(body) => Ok(PrivateExportResponse::Ready(Box::new(PrivateExportView {
                schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                format: "text/markdown;charset=utf-8".to_owned(),
                body,
                canonical_digest: None,
            }))),
            Err(error) => Ok(PrivateExportResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

#[server(input = Json)]
pub async fn generate_private_recap(
    intent: PrivateRecapIntent,
) -> Result<PrivateRecapResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(PrivateRecapResponse::Rejected(error)),
        };
        let command = manchester_dnd_server::GeneratePrivateRecapCommand {
            schema_version: intent.schema_version,
            campaign_session_id: LOCAL_CAMPAIGN_ID.to_owned(),
            expected_campaign_revision: intent.expected_campaign_revision,
            idempotency_key: intent.idempotency_key,
        };
        match context
            .application
            .generate_local_private_recap(command)
            .await
        {
            Ok(recap) => Ok(PrivateRecapResponse::Ready(Some(Box::new(recap_view(
                recap,
            ))))),
            Err(error) => Ok(PrivateRecapResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = intent;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn load_private_recap() -> Result<PrivateRecapResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(PrivateRecapResponse::Rejected(error)),
        };
        match context.application.load_local_private_recap().await {
            Ok(recap) => Ok(PrivateRecapResponse::Ready(
                recap.map(|value| Box::new(recap_view(value))),
            )),
            Err(error) => Ok(PrivateRecapResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the server-function macro replaces this body in browser builds")
}

#[server(input = Json)]
pub async fn prepare_campaign_delete(
    intent: PrepareDeleteIntent,
) -> Result<PrepareDeleteResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(PrepareDeleteResponse::Rejected(error)),
        };
        if intent.schema_version != LIFECYCLE_WIRE_SCHEMA_VERSION {
            return Ok(PrepareDeleteResponse::Rejected(invalid_wire_error(
                correlation_id,
            )));
        }
        match context
            .application
            .prepare_local_campaign_deletion(intent.expected_lifecycle_revision, intent.deletion_id)
            .await
        {
            Ok(prepared) => Ok(PrepareDeleteResponse::Ready(Box::new(PreparedDeleteView {
                schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                campaign_session_id: prepared.campaign_session_id,
                deletion_id: prepared.deletion_id,
                campaign_revision: prepared.campaign_revision,
                lifecycle_revision: prepared.lifecycle_revision,
                canonical_export_digest: prepared.canonical_export_digest.to_string(),
                canonical_export_json: prepared.canonical_export_json,
                expires_at: prepared.expires_at,
            }))),
            Err(error) => Ok(PrepareDeleteResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = intent;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server(input = Json)]
pub async fn delete_campaign(
    intent: DeleteCampaignIntent,
) -> Result<LifecycleMutationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::DeleteCampaignCommand;
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(LifecycleMutationResponse::Rejected(error)),
        };
        let command = DeleteCampaignCommand {
            lifecycle: trusted_lifecycle(intent.lifecycle),
            deletion_id: intent.deletion_id,
            confirm_permanent_delete: intent.confirm_permanent_delete,
        };
        match context.application.delete_local_campaign(command).await {
            Ok(outcome) => Ok(LifecycleMutationResponse::Committed(outcome_view(outcome))),
            Err(error) => Ok(LifecycleMutationResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = intent;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "ssr"), allow(dead_code))]
enum LifecycleAction {
    Archive,
    Restore,
}

#[cfg_attr(not(feature = "ssr"), allow(dead_code))]
async fn mutate_lifecycle(
    intent: LifecycleIntent,
    action: LifecycleAction,
) -> Result<LifecycleMutationResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let (context, correlation_id) = match authorized_context().await {
            Ok(value) => value,
            Err(error) => return Ok(LifecycleMutationResponse::Rejected(error)),
        };
        let command = trusted_lifecycle(intent);
        let result = match action {
            LifecycleAction::Archive => context.application.archive_local_campaign(command).await,
            LifecycleAction::Restore => {
                context
                    .application
                    .restore_local_campaign_from_archive(command)
                    .await
            }
        };
        match result {
            Ok(outcome) => Ok(LifecycleMutationResponse::Committed(outcome_view(outcome))),
            Err(error) => Ok(LifecycleMutationResponse::Rejected(
                crate::campaign::public_error(&error, correlation_id),
            )),
        }
    }
    #[cfg(not(feature = "ssr"))]
    {
        let _ = (intent, action);
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(feature = "ssr")]
async fn authorized_context()
-> Result<(manchester_dnd_server::ServerContext, String), PublicGameError> {
    let headers = crate::campaign::request_headers().await;
    let correlation_id = crate::campaign::request_correlation_id(headers.as_ref());
    if headers
        .as_ref()
        .is_none_or(|headers| !crate::campaign::headers_are_same_origin(headers))
    {
        return Err(crate::campaign::invalid_origin_error(correlation_id));
    }
    use_context::<manchester_dnd_server::ServerContext>()
        .map(|context| (context, correlation_id.clone()))
        .ok_or_else(|| crate::campaign::internal_error(correlation_id))
}

#[cfg(feature = "ssr")]
fn trusted_lifecycle(intent: LifecycleIntent) -> manchester_dnd_server::CampaignLifecycleCommand {
    manchester_dnd_server::CampaignLifecycleCommand {
        schema_version: intent.schema_version,
        campaign_session_id: intent.campaign_session_id,
        expected_lifecycle_revision: intent.expected_lifecycle_revision,
        idempotency_key: intent.idempotency_key,
    }
}

#[cfg(feature = "ssr")]
fn summary_view(summary: manchester_dnd_server::CampaignSummary) -> CampaignLifecycleView {
    CampaignLifecycleView {
        schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
        campaign_session_id: summary.campaign_session_id,
        title: summary.title,
        campaign_revision: summary.campaign_revision,
        lifecycle_revision: summary.lifecycle_revision,
        lifecycle_state: match summary.lifecycle_state {
            manchester_dnd_server::CampaignLifecycleState::Active => LifecycleStateView::Active,
            manchester_dnd_server::CampaignLifecycleState::Archived => LifecycleStateView::Archived,
        },
        archived_at: summary.archived_at,
        open_play_session_id: summary.open_play_session_id,
        retention_class: summary.retention_class,
        updated_at: summary.updated_at,
    }
}

#[cfg(feature = "ssr")]
fn outcome_view(outcome: manchester_dnd_server::CampaignLifecycleOutcome) -> LifecycleMutationView {
    LifecycleMutationView {
        schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
        campaign_session_id: outcome.campaign_session_id,
        lifecycle_revision: outcome.lifecycle_revision,
        lifecycle_state: outcome.lifecycle_state.map(|state| match state {
            manchester_dnd_server::CampaignLifecycleState::Active => LifecycleStateView::Active,
            manchester_dnd_server::CampaignLifecycleState::Archived => LifecycleStateView::Archived,
        }),
        play_session_id: outcome.play_session_id,
        deleted: outcome.deleted,
    }
}

#[cfg(feature = "ssr")]
fn recap_view(recap: manchester_dnd_server::CampaignPrivateRecap) -> PrivateRecapView {
    PrivateRecapView {
        schema_version: recap.schema_version,
        id: recap.id,
        campaign_revision: recap.campaign_revision,
        source_audit_count: recap.source_audit_count,
        source_audit_digest: recap.source_audit_digest.to_string(),
        template_id: recap.template_id,
        body: recap.body,
        body_digest: recap.body_digest.to_string(),
        created_at: recap.created_at,
    }
}

pub fn local_lifecycle_intent(view: &CampaignLifecycleView) -> LifecycleIntent {
    LifecycleIntent {
        schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
        campaign_session_id: LOCAL_CAMPAIGN_ID.to_owned(),
        expected_lifecycle_revision: view.lifecycle_revision,
        idempotency_key: uuid::Uuid::new_v4().to_string(),
    }
}

#[component]
pub fn CampaignLifecyclePanel(
    campaign_view: RwSignal<Option<manchester_dnd_core::LocalCampaignViewDto>>,
    campaign_loading: RwSignal<bool>,
    campaign_notice: RwSignal<String>,
) -> impl IntoView {
    let campaigns = RwSignal::new(Vec::<CampaignLifecycleView>::new());
    let pending = RwSignal::new(false);
    let notice = RwSignal::new("Loading campaign library…".to_owned());
    let history = RwSignal::new(Vec::<HistoryItemView>::new());
    let history_cursor = RwSignal::new(None::<u64>);
    let export_body = RwSignal::new(String::new());
    let export_label = RwSignal::new(String::new());
    let restore_body = RwSignal::new(String::new());
    let prepared_delete = RwSignal::new(None::<PreparedDeleteView>);
    let private_recap = RwSignal::new(None::<PrivateRecapView>);

    Effect::new(move |_| refresh_campaign_list(campaigns, pending, notice));
    // Gameplay, rewards, and advancement all move the campaign revision outside
    // this panel. Keep lifecycle mutations aligned with the already-validated
    // authoritative projection instead of submitting a stale revision captured
    // when the library first loaded.
    Effect::new(move |_| {
        let Some(authoritative) = campaign_view.get() else {
            return;
        };
        let needs_update = campaigns.with(|items| {
            items
                .first()
                .is_some_and(|campaign| campaign.campaign_revision != authoritative.revision)
        });
        if needs_update {
            campaigns.update(|items| {
                if let Some(campaign) = items.first_mut() {
                    campaign.campaign_revision = authoritative.revision;
                }
            });
        }
    });

    let create = move |_| {
        pending.set(true);
        notice.set("Creating the fixed local campaign…".to_owned());
        spawn_local(async move {
            match create_campaign().await {
                Ok(CampaignListResponse::Ready(items)) => {
                    campaigns.set(items);
                    notice.set("Campaign created and saved.".to_owned());
                    crate::load_campaign_into(campaign_view, campaign_loading, campaign_notice);
                }
                Ok(CampaignListResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set("Campaign creation was interrupted.".to_owned()),
            }
            pending.set(false);
        });
    };

    let resume = move |_| {
        campaign_notice.set("Reloading the authoritative campaign…".to_owned());
        crate::load_campaign_into(campaign_view, campaign_loading, campaign_notice);
    };

    let start_play = move |_| {
        let Some(campaign) = campaigns.get_untracked().into_iter().next() else {
            return;
        };
        pending.set(true);
        notice.set("Opening a durable play session…".to_owned());
        let intent = PlaySessionIntent {
            lifecycle: local_lifecycle_intent(&campaign),
            play_session_id: uuid::Uuid::new_v4().to_string(),
        };
        spawn_local(async move {
            match start_play_session(intent).await {
                Ok(LifecycleMutationResponse::Committed(_)) => {
                    notice.set(
                        "Play session opened. New turns will be saved under this sitting."
                            .to_owned(),
                    );
                    refresh_campaign_list(campaigns, pending, notice);
                }
                Ok(LifecycleMutationResponse::Rejected(error)) => {
                    notice.set(error.message);
                    pending.set(false);
                }
                Err(_) => {
                    notice.set("Play-session request was interrupted.".to_owned());
                    pending.set(false);
                }
            }
        });
    };

    let end_play = move |_| {
        let Some(campaign) = campaigns.get_untracked().into_iter().next() else {
            return;
        };
        let Some(play_session_id) = campaign.open_play_session_id.clone() else {
            return;
        };
        pending.set(true);
        notice.set("Closing this play session…".to_owned());
        let intent = PlaySessionIntent {
            lifecycle: local_lifecycle_intent(&campaign),
            play_session_id,
        };
        spawn_local(async move {
            match end_play_session(intent).await {
                Ok(LifecycleMutationResponse::Committed(_)) => {
                    notice.set("Play session closed at the saved campaign revision.".to_owned());
                    refresh_campaign_list(campaigns, pending, notice);
                }
                Ok(LifecycleMutationResponse::Rejected(error)) => {
                    notice.set(error.message);
                    pending.set(false);
                }
                Err(_) => {
                    notice.set("Play-session request was interrupted.".to_owned());
                    pending.set(false);
                }
            }
        });
    };

    let archive = move |_| {
        let Some(campaign) = campaigns.get_untracked().into_iter().next() else {
            return;
        };
        pending.set(true);
        notice.set("Archiving the campaign without deleting it…".to_owned());
        spawn_local(async move {
            match archive_campaign(local_lifecycle_intent(&campaign)).await {
                Ok(LifecycleMutationResponse::Committed(_)) => {
                    campaign_view.set(None);
                    notice
                        .set("Campaign archived. Its private history remains retained.".to_owned());
                    refresh_campaign_list(campaigns, pending, notice);
                }
                Ok(LifecycleMutationResponse::Rejected(error)) => {
                    notice.set(error.message);
                    pending.set(false);
                }
                Err(_) => {
                    notice.set("Archive request was interrupted.".to_owned());
                    pending.set(false);
                }
            }
        });
    };

    let restore_archive = move |_| {
        let Some(campaign) = campaigns.get_untracked().into_iter().next() else {
            return;
        };
        pending.set(true);
        notice.set("Restoring the archived campaign…".to_owned());
        spawn_local(async move {
            match restore_archived_campaign(local_lifecycle_intent(&campaign)).await {
                Ok(LifecycleMutationResponse::Committed(_)) => {
                    notice
                        .set("Campaign restored. Start a new play session when ready.".to_owned());
                    refresh_campaign_list(campaigns, pending, notice);
                    crate::load_campaign_into(campaign_view, campaign_loading, campaign_notice);
                }
                Ok(LifecycleMutationResponse::Rejected(error)) => {
                    notice.set(error.message);
                    pending.set(false);
                }
                Err(_) => {
                    notice.set("Restore request was interrupted.".to_owned());
                    pending.set(false);
                }
            }
        });
    };

    let load_history = move |_| {
        pending.set(true);
        let cursor = history_cursor.get_untracked();
        notice.set("Loading immutable stored turn audits…".to_owned());
        spawn_local(async move {
            match load_campaign_history(HistoryRequest {
                schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                after_turn_number: cursor,
                limit: 25,
            })
            .await
            {
                Ok(HistoryResponse::Ready(page)) => {
                    if cursor.is_none() {
                        history.set(page.items);
                    } else {
                        history.update(|items| items.extend(page.items));
                    }
                    history_cursor.set(page.next_after_turn_number);
                    notice.set("History rendered from saved audits only.".to_owned());
                }
                Ok(HistoryResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set("History request was interrupted.".to_owned()),
            }
            pending.set(false);
        });
    };

    let canonical_export = move |_| {
        pending.set(true);
        notice.set("Building the canonical private export…".to_owned());
        spawn_local(async move {
            match export_canonical_campaign().await {
                Ok(PrivateExportResponse::Ready(exported)) => {
                    export_label.set(format!(
                        "Canonical restorable JSON · {}",
                        exported
                            .canonical_digest
                            .as_deref()
                            .unwrap_or("digest unavailable")
                    ));
                    export_body.set(exported.body);
                    notice.set(
                        "Canonical private export is ready to copy to protected storage."
                            .to_owned(),
                    );
                }
                Ok(PrivateExportResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set("Export request was interrupted.".to_owned()),
            }
            pending.set(false);
        });
    };

    let readable_export = move |_| {
        pending.set(true);
        notice.set("Building the player-readable private record…".to_owned());
        spawn_local(async move {
            match export_readable_campaign().await {
                Ok(PrivateExportResponse::Ready(exported)) => {
                    export_label.set("Player-readable private Markdown".to_owned());
                    export_body.set(exported.body);
                    notice.set("Player-readable private record is ready.".to_owned());
                }
                Ok(PrivateExportResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set("Export request was interrupted.".to_owned()),
            }
            pending.set(false);
        });
    };

    let build_private_recap = move |_| {
        let Some(campaign) = campaigns.get_untracked().into_iter().next() else {
            return;
        };
        pending.set(true);
        notice.set("Building a private recap from committed audits…".to_owned());
        spawn_local(async move {
            match generate_private_recap(PrivateRecapIntent {
                schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                expected_campaign_revision: campaign.campaign_revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .await
            {
                Ok(PrivateRecapResponse::Ready(Some(recap))) => {
                    private_recap.set(Some(*recap));
                    notice
                        .set("Private recap saved with its committed-audit provenance.".to_owned());
                }
                Ok(PrivateRecapResponse::Ready(None)) => {
                    notice.set("No private recap was produced.".to_owned());
                }
                Ok(PrivateRecapResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set(
                    "The recap response was interrupted. Reloading cannot change saved mechanics."
                        .to_owned(),
                ),
            }
            pending.set(false);
        });
    };

    let load_saved_recap = move |_| {
        pending.set(true);
        notice.set("Loading the latest owner-private recap…".to_owned());
        spawn_local(async move {
            match load_private_recap().await {
                Ok(PrivateRecapResponse::Ready(Some(recap))) => {
                    private_recap.set(Some(*recap));
                    notice.set("Saved private recap loaded from PostgreSQL.".to_owned());
                }
                Ok(PrivateRecapResponse::Ready(None)) => {
                    private_recap.set(None);
                    notice.set("No private recap has been saved yet.".to_owned());
                }
                Ok(PrivateRecapResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set("Private recap loading was interrupted.".to_owned()),
            }
            pending.set(false);
        });
    };

    let prepare_delete = move |_| {
        let Some(campaign) = campaigns.get_untracked().into_iter().next() else {
            return;
        };
        pending.set(true);
        notice.set("Preparing a final canonical export before deletion…".to_owned());
        spawn_local(async move {
            match prepare_campaign_delete(PrepareDeleteIntent {
                schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                expected_lifecycle_revision: campaign.lifecycle_revision,
                deletion_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            {
                Ok(PrepareDeleteResponse::Ready(prepared)) => {
                    export_label.set(format!(
                        "Required pre-delete canonical export · {}",
                        prepared.canonical_export_digest
                    ));
                    export_body.set(prepared.canonical_export_json.clone());
                    prepared_delete.set(Some(*prepared));
                    notice.set("Save the export below, then use the separate permanent-delete confirmation.".to_owned());
                }
                Ok(PrepareDeleteResponse::Rejected(error)) => notice.set(error.message),
                Err(_) => notice.set("Delete preparation was interrupted.".to_owned()),
            }
            pending.set(false);
        });
    };

    let confirm_delete = move |_| {
        let Some(prepared) = prepared_delete.get_untracked() else {
            return;
        };
        pending.set(true);
        notice.set("Permanently deleting campaign-owned live data…".to_owned());
        spawn_local(async move {
            match delete_campaign(DeleteCampaignIntent {
                lifecycle: LifecycleIntent {
                    schema_version: LIFECYCLE_WIRE_SCHEMA_VERSION,
                    campaign_session_id: LOCAL_CAMPAIGN_ID.to_owned(),
                    expected_lifecycle_revision: prepared.lifecycle_revision,
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                },
                deletion_id: prepared.deletion_id,
                confirm_permanent_delete: true,
            })
            .await
            {
                Ok(LifecycleMutationResponse::Committed(outcome)) if outcome.deleted => {
                    campaign_view.set(None);
                    prepared_delete.set(None);
                    history.set(Vec::new());
                    history_cursor.set(None);
                    private_recap.set(None);
                    notice.set(
                        "Campaign deleted. A minimized deletion tombstone remains for 35 days."
                            .to_owned(),
                    );
                    refresh_campaign_list(campaigns, pending, notice);
                }
                Ok(LifecycleMutationResponse::Committed(_)) => {
                    notice.set("Delete did not reach a terminal state.".to_owned());
                    pending.set(false);
                }
                Ok(LifecycleMutationResponse::Rejected(error)) => {
                    notice.set(error.message);
                    pending.set(false);
                }
                Err(_) => {
                    notice.set("Delete request was interrupted; retry uses a new UI request after reloading state.".to_owned());
                    pending.set(false);
                }
            }
        });
    };

    let restore_export = move |_| {
        let body = restore_body.get_untracked();
        if body.trim().is_empty() {
            notice.set("Paste a canonical private export first.".to_owned());
            return;
        }
        pending.set(true);
        notice.set("Validating and restoring the canonical private export…".to_owned());
        spawn_local(async move {
            match restore_via_dedicated_route(&body, &uuid::Uuid::new_v4().to_string()).await {
                Ok(()) => {
                    restore_body.set(String::new());
                    notice.set("Campaign restored. Any previously open sitting was closed; start a new one.".to_owned());
                    refresh_campaign_list(campaigns, pending, notice);
                    crate::load_campaign_into(campaign_view, campaign_loading, campaign_notice);
                }
                Err(message) => {
                    notice.set(message);
                    pending.set(false);
                }
            }
        });
    };

    view! {
        <section class="lifecycle-panel" id="campaigns" aria-labelledby="campaigns-heading">
            <div class="panel-heading">
                <div>
                    <p class="eyebrow">"PRIVATE CAMPAIGN LIBRARY"</p>
                    <h2 id="campaigns-heading">"Save, resume, and export"</h2>
                </div>
                <span class="status-pill">"Local owner"</span>
            </div>
            <p>
                "Campaign lifecycle changes are revision-checked and saved in PostgreSQL. Archives do not expire automatically."
            </p>
            <p class="save-status" role="status" aria-live="polite" aria-busy=move || pending.get()>
                {move || notice.get()}
            </p>
            <button
                class="refresh-button"
                disabled=move || pending.get()
                on:click=move |_| refresh_campaign_list(campaigns, pending, notice)
            >
                "Reload campaign list"
            </button>

            <Show
                when=move || campaigns.get().is_empty()
                fallback=move || {
                    let campaign = campaigns.get().into_iter().next().expect("non-empty campaign list");
                    let active = campaign.lifecycle_state == LifecycleStateView::Active;
                    let open_play = campaign.open_play_session_id.is_some();
                    view! {
                        <article class="campaign-library-card">
                            <h3>{campaign.title}</h3>
                            <p>{format!(
                                "{} · campaign revision {} · lifecycle revision {}",
                                if active { "Active" } else { "Archived" },
                                campaign.campaign_revision,
                                campaign.lifecycle_revision,
                            )}</p>
                            <div class="lifecycle-actions">
                                <button disabled=move || pending.get() || !active on:click=resume>"Resume"</button>
                                <Show when=move || active && !open_play>
                                    <button disabled=move || pending.get() on:click=start_play>"Start play session"</button>
                                </Show>
                                <Show when=move || active && open_play>
                                    <button disabled=move || pending.get() on:click=end_play>"End play session"</button>
                                </Show>
                                <Show when=move || active && !open_play>
                                    <button disabled=move || pending.get() on:click=archive>"Archive"</button>
                                </Show>
                                <Show when=move || !active>
                                    <button disabled=move || pending.get() on:click=restore_archive>"Restore archive"</button>
                                </Show>
                                <button disabled=move || pending.get() on:click=load_history>"Load history"</button>
                                <button disabled=move || pending.get() on:click=build_private_recap>"Build/update private recap"</button>
                                <button disabled=move || pending.get() on:click=load_saved_recap>"Load saved private recap"</button>
                                <button disabled=move || pending.get() on:click=readable_export>"Readable export"</button>
                                <button disabled=move || pending.get() on:click=canonical_export>"Canonical export"</button>
                                <Show when=move || !active>
                                    <button class="danger-button" disabled=move || pending.get() on:click=prepare_delete>
                                        "Prepare permanent delete"
                                    </button>
                                </Show>
                            </div>
                        </article>
                    }
                }
            >
                <button class="primary-button" disabled=move || pending.get() on:click=create>
                    "Create local campaign"
                </button>
            </Show>

            <Show when=move || prepared_delete.get().is_some()>
                <div class="delete-confirmation" role="alert">
                    <strong>"Permanent delete is ready."</strong>
                    <p>"First copy the canonical export shown below. This removes live database rows; protected media backup limits are documented separately."</p>
                    <button class="danger-button" disabled=move || pending.get() on:click=confirm_delete>
                        "Confirm permanent delete"
                    </button>
                </div>
            </Show>

            <Show when=move || !history.get().is_empty()>
                <div class="campaign-history" aria-label="Stored turn history">
                    <h3>"Immutable turn history"</h3>
                    {move || history.get().into_iter().map(|item| {
                        let facts = serde_json::to_string_pretty(&item.event.payload)
                            .unwrap_or_else(|_| "Stored audit could not be rendered.".to_owned());
                        view! {
                            <details>
                                <summary>{format!("Turn {}", item.turn_number)}</summary>
                                <pre>{facts}</pre>
                            </details>
                        }
                    }).collect_view()}
                    <Show when=move || history_cursor.get().is_some()>
                        <button disabled=move || pending.get() on:click=load_history>"Load next page"</button>
                    </Show>
                </div>
            </Show>

            <Show when=move || private_recap.get().is_some()>
                {move || private_recap.get().map(|recap| view! {
                    <article class="private-recap" aria-label="Saved private campaign recap">
                        <h3>"Private campaign recap"</h3>
                        <p>{format!(
                            "Revision {} · {} committed audits · {} · body {}",
                            recap.campaign_revision,
                            recap.source_audit_count,
                            recap.template_id,
                            recap.body_digest,
                        )}</p>
                        <pre>{recap.body}</pre>
                    </article>
                })}
            </Show>

            <Show when=move || !export_body.get().is_empty()>
                <label class="private-export-field">
                    <span>{move || export_label.get()}</span>
                    <textarea rows="12" readonly prop:value=move || export_body.get()></textarea>
                </label>
            </Show>

            <details class="restore-export">
                <summary>"Restore a canonical private export"</summary>
                <p>"Only the strict versioned private format is accepted. Unknown schemas and altered pins fail closed."</p>
                <label>
                    <span>"Canonical export JSON"</span>
                    <textarea
                        rows="8"
                        prop:value=move || restore_body.get()
                        on:input=move |event| restore_body.set(event_target_value(&event))
                    ></textarea>
                </label>
                <button disabled=move || pending.get() on:click=restore_export>"Validate and restore"</button>
            </details>
        </section>
    }
}

fn refresh_campaign_list(
    campaigns: RwSignal<Vec<CampaignLifecycleView>>,
    pending: RwSignal<bool>,
    notice: RwSignal<String>,
) {
    pending.set(true);
    spawn_local(async move {
        match list_campaigns().await {
            Ok(CampaignListResponse::Ready(items)) => {
                let count = items.len();
                campaigns.set(items);
                notice.set(if count == 0 {
                    "No local campaign exists. Create one or restore a canonical export.".to_owned()
                } else {
                    "Campaign library loaded from durable storage.".to_owned()
                });
            }
            Ok(CampaignListResponse::Rejected(error)) => notice.set(error.message),
            Err(_) => notice.set("Campaign library request was interrupted.".to_owned()),
        }
        pending.set(false);
    });
}

#[cfg(feature = "hydrate")]
async fn restore_via_dedicated_route(
    canonical_export_json: &str,
    idempotency_key: &str,
) -> Result<(), String> {
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Request, RequestInit, Response};

    let options = RequestInit::new();
    options.set_method("POST");
    options.set_body(&JsValue::from_str(canonical_export_json));
    let request = Request::new_with_str_and_init("/api/local/campaign/restore", &options)
        .map_err(|_| "The restore request could not be created.".to_owned())?;
    request
        .headers()
        .set(
            "content-type",
            "application/vnd.manchester-arcana.campaign+json;version=1",
        )
        .map_err(|_| "The restore request content type could not be set.".to_owned())?;
    request
        .headers()
        .set("x-manchester-arcana-restore", "1")
        .map_err(|_| "The restore confirmation header could not be set.".to_owned())?;
    request
        .headers()
        .set("idempotency-key", idempotency_key)
        .map_err(|_| "The restore idempotency key could not be set.".to_owned())?;
    let window = web_sys::window().ok_or_else(|| "Browser window is unavailable.".to_owned())?;
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| "The restore request was interrupted.".to_owned())?
        .dyn_into::<Response>()
        .map_err(|_| "The restore response was invalid.".to_owned())?;
    if response.ok() {
        Ok(())
    } else {
        Err(match response.status() {
            400 => "The export is malformed, non-canonical, or incompatible.".to_owned(),
            403 => "The restore request was not authorized for this local origin.".to_owned(),
            409 => "A campaign already exists or the restore key conflicts.".to_owned(),
            413 => "The canonical export exceeds the 2 MiB restore limit.".to_owned(),
            _ => "The campaign could not be restored.".to_owned(),
        })
    }
}

#[cfg(not(feature = "hydrate"))]
async fn restore_via_dedicated_route(
    _canonical_export_json: &str,
    _idempotency_key: &str,
) -> Result<(), String> {
    Err("Restore is available after the browser app hydrates.".to_owned())
}

#[cfg_attr(not(feature = "ssr"), allow(dead_code))]
fn invalid_wire_error(correlation_id: String) -> PublicGameError {
    PublicGameError {
        code: "invalid_campaign_lifecycle".to_owned(),
        message: "That campaign lifecycle action is invalid.".to_owned(),
        retryable: false,
        current_revision: None,
        correlation_id,
        alternatives: Vec::new(),
    }
}
