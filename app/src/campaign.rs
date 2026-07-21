use leptos::prelude::*;
use manchester_dnd_core::{
    AttemptExplorationCheckCommand, ExplorationCheckOutcomeDto, LocalCampaignViewDto,
};
use serde::{Deserialize, Serialize};

pub const LOCAL_EXPLORATION_ACTION_ID: &str = "inspect-viaduct-runes";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicGameError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub current_revision: Option<u64>,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum CampaignLoadResponse {
    Ready(LocalCampaignViewDto),
    Rejected(PublicGameError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum ExplorationCheckResponse {
    Committed(ExplorationCheckOutcomeDto),
    Rejected(PublicGameError),
}

#[server]
pub async fn load_local_campaign() -> Result<CampaignLoadResponse, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use manchester_dnd_server::ServerContext;

        if !request_is_same_origin().await {
            return Ok(CampaignLoadResponse::Rejected(invalid_origin_error()));
        }
        let Some(context) = use_context::<ServerContext>() else {
            return Ok(CampaignLoadResponse::Rejected(internal_error()));
        };
        match context.application.load_local_campaign().await {
            Ok(view) => Ok(CampaignLoadResponse::Ready(view)),
            Err(error) => Ok(CampaignLoadResponse::Rejected(public_error(&error))),
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

        if !request_is_same_origin().await {
            return Ok(ExplorationCheckResponse::Rejected(invalid_origin_error()));
        }

        let Some(context) = use_context::<ServerContext>() else {
            return Ok(ExplorationCheckResponse::Rejected(internal_error()));
        };
        match context.application.attempt_exploration_check(command).await {
            Ok(outcome) => Ok(ExplorationCheckResponse::Committed(outcome)),
            Err(error) => Ok(ExplorationCheckResponse::Rejected(public_error(&error))),
        }
    }

    #[cfg(not(feature = "ssr"))]
    {
        let _ = command;
        unreachable!("the server-function macro replaces this body in browser builds")
    }
}

#[cfg(feature = "ssr")]
fn public_error(error: &manchester_dnd_server::ApplicationError) -> PublicGameError {
    let correlation_id = uuid::Uuid::new_v4().to_string();
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
    }
}

#[cfg(feature = "ssr")]
fn internal_error() -> PublicGameError {
    let correlation_id = uuid::Uuid::new_v4().to_string();
    tracing::error!(correlation_id, "server context unavailable");
    PublicGameError {
        code: "internal_error".to_owned(),
        message: "The game service is temporarily unavailable.".to_owned(),
        retryable: true,
        current_revision: None,
        correlation_id,
    }
}

#[cfg(feature = "ssr")]
fn invalid_origin_error() -> PublicGameError {
    PublicGameError {
        code: "invalid_request_origin".to_owned(),
        message: "The request must come from this local game page.".to_owned(),
        retryable: false,
        current_revision: None,
        correlation_id: uuid::Uuid::new_v4().to_string(),
    }
}

#[cfg(feature = "ssr")]
async fn request_is_same_origin() -> bool {
    leptos_axum::extract::<http::HeaderMap>()
        .await
        .is_ok_and(|headers| headers_are_same_origin(&headers))
}

#[cfg(feature = "ssr")]
fn headers_are_same_origin(headers: &http::HeaderMap) -> bool {
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

    use super::headers_are_same_origin;

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
}
