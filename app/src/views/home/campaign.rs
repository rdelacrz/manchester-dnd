use leptos::prelude::*;
use manchester_dnd_core::{
    AdvanceNpcTurnCommand, AttemptExplorationCheckCommand, AttemptSocialInteractionCommand,
    CommitEncounterCommand, CommittedEncounterOutcomeDto, ExplorationCheckOutcomeDto,
    LocalCampaignViewDto, SocialInteractionOutcomeDto,
};
use serde::{Deserialize, Serialize};

pub const LOCAL_EXPLORATION_ACTION_ID: &str = "inspect-viaduct-runes";
pub const LOCAL_SOCIAL_ACTION_ID: &str = "parley-lockkeeper";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicGameError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub current_revision: Option<u64>,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<manchester_dnd_core::hero::AuthoredAlternative>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CampaignLoadResponse {
    Ready(Box<LocalCampaignViewDto>),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum ExplorationCheckResponse {
    Committed(ExplorationCheckOutcomeDto),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum SocialInteractionResponse {
    Committed(SocialInteractionOutcomeDto),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum EncounterCommandResponse {
    Committed(Box<CommittedEncounterOutcomeDto>),
    Rejected(PublicGameError),
}

#[server]
pub async fn load_local_campaign() -> Result<CampaignLoadResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = request_headers().await;
        let correlation_id = request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !headers_are_same_origin(headers))
        {
            return Ok(CampaignLoadResponse::Rejected(invalid_origin_error(
                correlation_id,
            )));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(CampaignLoadResponse::Rejected(internal_error(
                correlation_id,
            )));
        };
        match context.application.load_local_campaign().await {
            Ok(view) => Ok(CampaignLoadResponse::Ready(Box::new(view))),
            Err(error) => Ok(CampaignLoadResponse::Rejected(public_error(
                &error,
                correlation_id,
            ))),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[server]
pub async fn attempt_exploration_check(
    command: AttemptExplorationCheckCommand,
) -> Result<ExplorationCheckResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = request_headers().await;
        let correlation_id = request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !headers_are_same_origin(headers))
        {
            return Ok(ExplorationCheckResponse::Rejected(invalid_origin_error(
                correlation_id,
            )));
        }

        let Some(context) = use_context::<ServerContext>() else {
            return Ok(ExplorationCheckResponse::Rejected(internal_error(
                correlation_id,
            )));
        };
        match context
            .application
            .attempt_exploration_check_with_correlation(command, &correlation_id)
            .await
        {
            Ok(outcome) => Ok(ExplorationCheckResponse::Committed(outcome)),
            Err(error) => Ok(ExplorationCheckResponse::Rejected(public_error(
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
pub async fn attempt_social_interaction(
    command: AttemptSocialInteractionCommand,
) -> Result<SocialInteractionResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = request_headers().await;
        let correlation_id = request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !headers_are_same_origin(headers))
        {
            return Ok(SocialInteractionResponse::Rejected(invalid_origin_error(
                correlation_id,
            )));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(SocialInteractionResponse::Rejected(internal_error(
                correlation_id,
            )));
        };
        match context
            .application
            .attempt_social_interaction_with_correlation(command, &correlation_id)
            .await
        {
            Ok(outcome) => Ok(SocialInteractionResponse::Committed(outcome)),
            Err(error) => Ok(SocialInteractionResponse::Rejected(public_error(
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
pub async fn submit_encounter_action(
    command: CommitEncounterCommand,
) -> Result<EncounterCommandResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = request_headers().await;
        let correlation_id = request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !headers_are_same_origin(headers))
        {
            return Ok(EncounterCommandResponse::Rejected(invalid_origin_error(
                correlation_id,
            )));
        }

        let Some(context) = use_context::<ServerContext>() else {
            return Ok(EncounterCommandResponse::Rejected(internal_error(
                correlation_id,
            )));
        };
        match context
            .application
            .commit_encounter_command_with_correlation(command, &correlation_id)
            .await
        {
            Ok(outcome) => Ok(EncounterCommandResponse::Committed(Box::new(outcome))),
            Err(error) => Ok(EncounterCommandResponse::Rejected(public_error(
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
pub async fn advance_npc_turn(
    command: AdvanceNpcTurnCommand,
) -> Result<EncounterCommandResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        let headers = request_headers().await;
        let correlation_id = request_correlation_id(headers.as_ref());
        if headers
            .as_ref()
            .is_none_or(|headers| !headers_are_same_origin(headers))
        {
            return Ok(EncounterCommandResponse::Rejected(invalid_origin_error(
                correlation_id,
            )));
        }

        let Some(context) = use_context::<ServerContext>() else {
            return Ok(EncounterCommandResponse::Rejected(internal_error(
                correlation_id,
            )));
        };
        match context
            .application
            .advance_npc_turn_with_correlation(command, &correlation_id)
            .await
        {
            Ok(outcome) => Ok(EncounterCommandResponse::Committed(Box::new(outcome))),
            Err(error) => Ok(EncounterCommandResponse::Rejected(public_error(
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
pub(crate) fn public_error(
    error: &manchester_dnd_server::ApplicationError,
    correlation_id: String,
) -> PublicGameError {
    tracing::warn!(
        correlation_id,
        code = error.public_code(),
        "game command rejected"
    );
    PublicGameError {
        code: error.public_code().to_owned(),
        message: error.safe_message().to_owned(),
        retryable: error.retryable(),
        current_revision: error.current_revision(),
        correlation_id,
        alternatives: error
            .unsupported_hero_mechanic()
            .map(|unsupported| unsupported.alternatives.clone())
            .unwrap_or_default(),
    }
}

#[cfg(feature = "ssr")]
pub(crate) fn internal_error(correlation_id: String) -> PublicGameError {
    tracing::error!(correlation_id, "server context unavailable");
    PublicGameError {
        code: "internal_error".to_owned(),
        message: "The game service is temporarily unavailable.".to_owned(),
        retryable: true,
        current_revision: None,
        correlation_id,
        alternatives: Vec::new(),
    }
}

#[cfg(feature = "ssr")]
pub(crate) fn invalid_origin_error(correlation_id: String) -> PublicGameError {
    PublicGameError {
        code: "invalid_request_origin".to_owned(),
        message: "The request must come from this local game page.".to_owned(),
        retryable: false,
        current_revision: None,
        correlation_id,
        alternatives: Vec::new(),
    }
}

#[cfg(feature = "ssr")]
pub(crate) async fn request_headers() -> Option<http::HeaderMap> {
    leptos_axum::extract::<http::HeaderMap>().await.ok()
}

#[cfg(feature = "ssr")]
pub(crate) fn request_correlation_id(headers: Option<&http::HeaderMap>) -> String {
    headers
        .and_then(|headers| headers.get("x-correlation-id"))
        .and_then(|value| value.to_str().ok())
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

#[cfg(feature = "ssr")]
pub(crate) fn headers_are_same_origin(headers: &http::HeaderMap) -> bool {
    use http::header::{HOST, ORIGIN};
    use http::uri::Authority;

    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = origin.parse::<http::Uri>() else {
        return false;
    };
    let Ok(request_authority) = host.parse::<Authority>() else {
        return false;
    };
    origin.scheme_str() == Some("http")
        && origin.authority().is_some_and(|origin_authority| {
            origin_authority
                .as_str()
                .eq_ignore_ascii_case(request_authority.as_str())
                && authority_is_loopback(origin_authority)
        })
}

#[cfg(feature = "ssr")]
fn authority_is_loopback(authority: &http::uri::Authority) -> bool {
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::{headers_are_same_origin, request_correlation_id};

    #[test]
    fn mutation_origin_must_match_the_request_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:6789"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:6789"),
        );
        assert!(headers_are_same_origin(&headers));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://malicious.example"),
        );
        assert!(!headers_are_same_origin(&headers));
        headers.remove(header::ORIGIN);
        assert!(!headers_are_same_origin(&headers));

        headers.insert(header::HOST, HeaderValue::from_static("malicious.example"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://malicious.example"),
        );
        assert!(!headers_are_same_origin(&headers));

        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:6789"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://127.0.0.1:6789"),
        );
        assert!(!headers_are_same_origin(&headers));
    }

    #[test]
    fn valid_http_correlation_id_is_reused_and_untrusted_values_are_replaced() {
        let expected = uuid::Uuid::new_v4().to_string();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-correlation-id",
            HeaderValue::from_str(&expected).unwrap(),
        );
        assert_eq!(request_correlation_id(Some(&headers)), expected);

        headers.insert(
            "x-correlation-id",
            HeaderValue::from_static("not-a-correlation-id"),
        );
        assert!(uuid::Uuid::parse_str(&request_correlation_id(Some(&headers))).is_ok());
    }
}
