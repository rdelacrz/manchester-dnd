use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::{Client, Response, header, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::{
    config::{LlmBackend, LlmProfile, SecretString},
    error::GenerationError,
};

const MAX_TEXT_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEXT_REQUEST_BYTES: usize = 256 * 1024;
const MAX_IMAGE_PROMPT_CHARS: usize = 16_000;
const MAX_IMAGES_PER_REQUEST: u8 = 4;
const MAX_PROVIDER_OPTION_CHARS: usize = 64;
const MAX_OUTPUT_TOKENS: u32 = 128_000;
const MAX_DECODED_IMAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextResponseFormat {
    Text,
    JsonObject,
}

#[derive(Debug, Clone)]
pub struct TextGenerationRequest {
    pub messages: Vec<ChatMessage>,
    pub response_format: TextResponseFormat,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextGenerationResponse {
    pub text: String,
    /// Adapter-authoritative configured model identity, never copied directly
    /// from an untrusted provider response.
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub count: u8,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub style: Option<String>,
}

impl ImageGenerationRequest {
    pub fn one(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            count: 1,
            size: None,
            quality: None,
            style: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub url: Option<String>,
    pub base64_data: Option<String>,
    pub revised_prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerationResponse {
    pub images: Vec<GeneratedImage>,
}

#[async_trait]
pub trait TextGenerator: Send + Sync {
    async fn generate_text(
        &self,
        request: TextGenerationRequest,
    ) -> Result<TextGenerationResponse, GenerationError>;
}

#[async_trait]
pub trait ImageGenerator: Send + Sync {
    async fn generate_image(
        &self,
        request: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, GenerationError>;
}

#[derive(Clone)]
pub struct OpenAiCompatibleGenerator {
    client: Client,
    base_url: Url,
    api_key: Option<SecretString>,
    model: String,
    timeout: Duration,
    default_max_output_tokens: Option<u32>,
    default_temperature: Option<f32>,
    default_image_size: Option<String>,
}

impl fmt::Debug for OpenAiCompatibleGenerator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleGenerator")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("default_temperature", &self.default_temperature)
            .field("default_image_size", &self.default_image_size)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleGenerator {
    pub fn new(profile: &LlmProfile) -> Result<Self, GenerationError> {
        if profile.backend != LlmBackend::OpenAiCompatible {
            return Err(GenerationError::InvalidConfiguration(
                "adapter requires an openai-compatible profile".to_owned(),
            ));
        }
        let base_url = profile.base_url.clone().ok_or_else(|| {
            GenerationError::InvalidConfiguration("profile has no base URL".to_owned())
        })?;
        let model = profile.model.clone().ok_or_else(|| {
            GenerationError::InvalidConfiguration("profile has no model".to_owned())
        })?;
        let safe_http_host = base_url.host().is_some_and(|host| match host {
            url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        });
        let unsafe_direct_ip = base_url.host().is_some_and(|host| match host {
            url::Host::Ipv4(address) => !address.is_loopback(),
            url::Host::Ipv6(address) => !address.is_loopback(),
            url::Host::Domain(_) => false,
        });
        if !matches!(base_url.scheme(), "http" | "https")
            || (base_url.scheme() == "http" && !safe_http_host)
            || unsafe_direct_ip
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || model.trim().is_empty()
            || model.chars().count() > 256
            || profile.timeout.is_zero()
            || profile.timeout > Duration::from_secs(600)
            || profile
                .max_output_tokens
                .is_some_and(|tokens| !(1..=MAX_OUTPUT_TOKENS).contains(&tokens))
            || profile
                .temperature
                .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
            || profile.default_image_size.as_ref().is_some_and(|value| {
                value.trim().is_empty() || value.chars().count() > MAX_PROVIDER_OPTION_CHARS
            })
        {
            return Err(GenerationError::InvalidConfiguration(
                "profile transport, model, timeout, or generation limits are invalid".to_owned(),
            ));
        }
        let client = Client::builder()
            .timeout(profile.timeout)
            // A redirect could move a bearer-authenticated request to an
            // endpoint outside the operator-approved model origin.
            .redirect(Policy::none())
            .build()
            .map_err(GenerationError::Transport)?;

        Ok(Self {
            client,
            base_url,
            api_key: profile.api_key.clone(),
            model,
            timeout: profile.timeout,
            default_max_output_tokens: profile.max_output_tokens,
            default_temperature: profile.temperature,
            default_image_size: profile.default_image_size.clone(),
        })
    }

    fn endpoint(&self, relative: &str) -> Result<Url, GenerationError> {
        let mut base = self.base_url.as_str().to_owned();
        if !base.ends_with('/') {
            base.push('/');
        }
        Url::parse(&base)
            .and_then(|url| url.join(relative))
            .map_err(|_| {
                GenerationError::InvalidConfiguration(
                    "profile base URL cannot form an API endpoint".to_owned(),
                )
            })
    }

    fn authenticated(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(api_key) => builder.bearer_auth(api_key.expose_secret()),
            None => builder,
        }
    }

    async fn checked_body(
        &self,
        mut response: Response,
        endpoint: &'static str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, GenerationError> {
        let status = response.status();
        if !status.is_success() {
            return Err(GenerationError::HttpStatus {
                status,
                request_id: request_id(&response),
            });
        }

        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(GenerationError::InvalidResponse {
                endpoint,
                reason: "body exceeded the configured safety limit",
            });
        }

        let mut body = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or_default()
                .min(max_bytes as u64) as usize,
        );
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                GenerationError::Timeout {
                    timeout: self.timeout,
                }
            } else {
                GenerationError::Transport(error)
            }
        })? {
            if body.len().saturating_add(chunk.len()) > max_bytes {
                return Err(GenerationError::InvalidResponse {
                    endpoint,
                    reason: "body exceeded the configured safety limit",
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<Response, GenerationError> {
        self.authenticated(builder)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    GenerationError::Timeout {
                        timeout: self.timeout,
                    }
                } else {
                    GenerationError::Transport(error)
                }
            })
    }
}

#[async_trait]
impl TextGenerator for OpenAiCompatibleGenerator {
    async fn generate_text(
        &self,
        request: TextGenerationRequest,
    ) -> Result<TextGenerationResponse, GenerationError> {
        validate_text_request(&request)?;
        let endpoint = self.endpoint("chat/completions")?;
        let body = chat_request_body(
            &self.model,
            &request,
            self.default_max_output_tokens,
            self.default_temperature,
        );
        let response = self.send(self.client.post(endpoint).json(&body)).await?;
        let bytes = self
            .checked_body(response, "chat completion", MAX_TEXT_RESPONSE_BYTES)
            .await?;
        let mut parsed = parse_chat_response(&bytes)?;
        parsed.model = Some(self.model.clone());
        Ok(parsed)
    }
}

#[async_trait]
impl ImageGenerator for OpenAiCompatibleGenerator {
    async fn generate_image(
        &self,
        request: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, GenerationError> {
        validate_image_request(&request)?;
        let endpoint = self.endpoint("images/generations")?;
        let body = image_request_body(&self.model, &request, self.default_image_size.as_deref());
        let response = self.send(self.client.post(endpoint).json(&body)).await?;
        let bytes = self
            .checked_body(response, "image generation", MAX_IMAGE_RESPONSE_BYTES)
            .await?;
        let parsed = parse_image_response(&bytes)?;
        if parsed.images.len() > usize::from(request.count) {
            return Err(GenerationError::InvalidResponse {
                endpoint: "image generation",
                reason: "provider returned more images than requested",
            });
        }
        Ok(parsed)
    }
}

fn validate_text_request(request: &TextGenerationRequest) -> Result<(), GenerationError> {
    let content_bytes = request
        .messages
        .iter()
        .map(|message| message.content.len())
        .try_fold(0usize, usize::checked_add);
    if request.messages.is_empty()
        || request.messages.len() > 256
        || request
            .messages
            .iter()
            .any(|message| message.content.trim().is_empty())
        || content_bytes.is_none_or(|bytes| bytes > MAX_TEXT_REQUEST_BYTES)
        || request
            .max_output_tokens
            .is_some_and(|tokens| !(1..=MAX_OUTPUT_TOKENS).contains(&tokens))
        || request
            .temperature
            .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
    {
        return Err(GenerationError::InvalidConfiguration(
            "text request messages, size, token limit, or temperature are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_image_request(request: &ImageGenerationRequest) -> Result<(), GenerationError> {
    if request.prompt.trim().is_empty()
        || request.prompt.chars().count() > MAX_IMAGE_PROMPT_CHARS
        || request.count == 0
        || request.count > MAX_IMAGES_PER_REQUEST
        || [&request.size, &request.quality, &request.style]
            .into_iter()
            .flatten()
            .any(|value| {
                value.trim().is_empty() || value.chars().count() > MAX_PROVIDER_OPTION_CHARS
            })
    {
        return Err(GenerationError::InvalidConfiguration(format!(
            "image prompt must contain 1 to {MAX_IMAGE_PROMPT_CHARS} characters and count must be between 1 and {MAX_IMAGES_PER_REQUEST}"
        )));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct DisabledTextGenerator;

#[async_trait]
impl TextGenerator for DisabledTextGenerator {
    async fn generate_text(
        &self,
        _request: TextGenerationRequest,
    ) -> Result<TextGenerationResponse, GenerationError> {
        Err(GenerationError::Disabled { capability: "text" })
    }
}

#[derive(Debug, Default)]
pub struct DisabledImageGenerator;

#[async_trait]
impl ImageGenerator for DisabledImageGenerator {
    async fn generate_image(
        &self,
        _request: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, GenerationError> {
        Err(GenerationError::Disabled {
            capability: "image",
        })
    }
}

/// Deterministic, network-free provider used by CI, demos, and degraded play.
#[derive(Debug, Default)]
pub struct FakeTextGenerator;

#[async_trait]
impl TextGenerator for FakeTextGenerator {
    async fn generate_text(
        &self,
        request: TextGenerationRequest,
    ) -> Result<TextGenerationResponse, GenerationError> {
        validate_text_request(&request)?;
        let text = match request.response_format {
            TextResponseFormat::Text => {
                "The rain eases. The committed facts remain unchanged, and the way ahead is clear."
                    .to_owned()
            }
            TextResponseFormat::JsonObject => fake_json_response(&request),
        };
        Ok(TextGenerationResponse {
            text,
            model: Some("deterministic-fake-v1".to_owned()),
            finish_reason: Some("stop".to_owned()),
            usage: TokenUsage::default(),
        })
    }
}

#[derive(Debug, Default)]
pub struct FakeImageGenerator;

#[async_trait]
impl ImageGenerator for FakeImageGenerator {
    async fn generate_image(
        &self,
        request: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, GenerationError> {
        validate_image_request(&request)?;
        // Valid metadata-free 1x1 PNG. The artifact worker still treats it as
        // untrusted and runs the normal decode/validation/storage pipeline.
        const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        Ok(ImageGenerationResponse {
            images: (0..request.count)
                .map(|_| GeneratedImage {
                    url: None,
                    base64_data: Some(PNG_1X1.to_owned()),
                    revised_prompt: None,
                })
                .collect(),
        })
    }
}

fn fake_json_response(request: &TextGenerationRequest) -> String {
    let envelope = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::User)
        .and_then(|message| serde_json::from_str::<Value>(&message.content).ok());
    if let Some(envelope) = envelope.as_ref()
        && envelope.get("required_base").is_some()
    {
        return fake_typed_gm_response(envelope);
    }
    let required = envelope.and_then(|value| value.get("required_output").cloned());
    let Some(required) = required else {
        return json!({
            "schema_version": 1,
            "status": "deterministic_fallback",
            "text": "No provider-specific output was required."
        })
        .to_string();
    };
    json!({
        "schema_version": required
            .get("proposal_schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(1),
        "proposal_id": required
            .get("proposal_id")
            .and_then(Value::as_str)
            .unwrap_or("fake:proposal"),
        "session_id": required
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("local-campaign"),
        "based_on_event_sequence": required
            .get("based_on_event_sequence")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "narrative": {
            "text": "The rain eases. The committed result stands, and the next safe choices are yours.",
            "image_prompt": null,
            "choices": []
        },
        "effects": []
    })
    .to_string()
}

fn fake_typed_gm_response(envelope: &Value) -> String {
    let base = envelope
        .get("required_base")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let task = envelope.get("task").and_then(Value::as_str).unwrap_or("");
    if task == "narrate_committed_facts" {
        let suffix = base
            .get("proposal_id")
            .and_then(Value::as_str)
            .and_then(|value| value.rsplit(':').next())
            .unwrap_or("fallback")
            .chars()
            .take(24)
            .collect::<String>();
        return json!({
            "type": "narration",
            "base": base,
            "narration_id": format!("fake:narration:{suffix}"),
            "text": "Rain catches the lantern light as the recorded result settles into the scene. The committed mechanics remain exactly as resolved.",
            "claimed_facts": envelope
                .get("authoritative_mechanical_facts")
                .cloned()
                .unwrap_or_else(|| json!([])),
        })
        .to_string();
    }

    let legal_ids = envelope
        .pointer("/legal_ids/action_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let public_facts = envelope
        .pointer("/untrusted_data/committed_public_facts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let intent = envelope
        .pointer("/untrusted_data/player_intent")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let wanted = [
        (["attack", "hit", "strike"].as_slice(), "attack"),
        (["sluice", "gate", "release"].as_slice(), "sluice"),
        (["move", "approach", "toward", "closer"].as_slice(), "move"),
        (["begin", "start", "initiative"].as_slice(), "initiative"),
        (["end", "wait", "pass"].as_slice(), "end the current turn"),
        (["death", "save"].as_slice(), "death save"),
    ]
    .into_iter()
    .find_map(|(intent_words, label_word)| {
        intent_words
            .iter()
            .any(|word| intent.contains(word))
            .then_some(label_word)
    });
    let selected = wanted.and_then(|label_word| {
        public_facts.iter().find_map(|fact| {
            let id = fact.get("fact_id")?.as_str()?;
            let summary = fact.get("summary")?.as_str()?.to_ascii_lowercase();
            let legal = legal_ids.iter().any(|legal| legal.as_str() == Some(id));
            (legal && summary.contains(label_word)).then_some(id.to_owned())
        })
    });
    let selected = selected.or_else(|| {
        (legal_ids.len() == 1)
            .then(|| {
                legal_ids
                    .first()
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .flatten()
    });
    if let Some(action_id) = selected {
        return json!({
            "type": "action",
            "base": base,
            "action_id": action_id,
            "target_id": null,
            "rationale": "The deterministic fake matched the player's words to one currently legal action ID.",
        })
        .to_string();
    }

    let choices = legal_ids
        .iter()
        .filter_map(Value::as_str)
        .take(4)
        .enumerate()
        .map(|(index, action_id)| {
            let label = public_facts
                .iter()
                .find(|fact| fact.get("fact_id").and_then(Value::as_str) == Some(action_id))
                .and_then(|fact| fact.get("summary"))
                .and_then(Value::as_str)
                .and_then(|summary| summary.strip_prefix("Currently legal action: "))
                .and_then(|summary| summary.split(". Use exactly action ID").next())
                .unwrap_or("Choose this legal action");
            json!({
                "choice_id": format!("fake:choice:{index}"),
                "label": label,
                "action_id": action_id,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "clarification",
        "base": base,
        "question": "Which of the current authored actions best matches what you want to do?",
        "choices": choices,
    })
    .to_string()
}

pub struct GenerationProviders {
    pub text: Arc<dyn TextGenerator>,
    pub image: Arc<dyn ImageGenerator>,
}

impl GenerationProviders {
    pub fn from_profiles(text: &LlmProfile, image: &LlmProfile) -> Result<Self, GenerationError> {
        Ok(Self {
            text: text_provider(text)?,
            image: image_provider(image)?,
        })
    }
}

pub fn text_provider(profile: &LlmProfile) -> Result<Arc<dyn TextGenerator>, GenerationError> {
    match profile.backend {
        LlmBackend::Disabled => Ok(Arc::new(DisabledTextGenerator)),
        LlmBackend::Fake => Ok(Arc::new(FakeTextGenerator)),
        LlmBackend::OpenAiCompatible => Ok(Arc::new(OpenAiCompatibleGenerator::new(profile)?)),
    }
}

pub fn image_provider(profile: &LlmProfile) -> Result<Arc<dyn ImageGenerator>, GenerationError> {
    match profile.backend {
        LlmBackend::Disabled => Ok(Arc::new(DisabledImageGenerator)),
        LlmBackend::Fake => Ok(Arc::new(FakeImageGenerator)),
        LlmBackend::OpenAiCompatible => Ok(Arc::new(OpenAiCompatibleGenerator::new(profile)?)),
    }
}

fn chat_request_body(
    model: &str,
    request: &TextGenerationRequest,
    default_max_output_tokens: Option<u32>,
    default_temperature: Option<f32>,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": request.messages,
    });
    let object = body
        .as_object_mut()
        .expect("json object literal must remain an object");
    if request.response_format == TextResponseFormat::JsonObject {
        object.insert(
            "response_format".to_owned(),
            json!({ "type": "json_object" }),
        );
    }
    if let Some(temperature) = request.temperature.or(default_temperature) {
        object.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(max_tokens) = request.max_output_tokens.or(default_max_output_tokens) {
        object.insert("max_tokens".to_owned(), json!(max_tokens));
    }
    body
}

fn image_request_body(
    model: &str,
    request: &ImageGenerationRequest,
    default_image_size: Option<&str>,
) -> Value {
    let mut body = json!({
        "model": model,
        "prompt": request.prompt,
        "n": request.count,
    });
    let object = body
        .as_object_mut()
        .expect("json object literal must remain an object");
    for (name, value) in [
        ("size", request.size.as_deref().or(default_image_size)),
        ("quality", request.quality.as_deref()),
        ("style", request.style.as_deref()),
    ] {
        if let Some(value) = value {
            object.insert(name.to_owned(), json!(value));
        }
    }
    body
}

#[derive(Deserialize)]
struct WireChatResponse {
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    content: WireContent,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Parts(Vec<WireContentPart>),
}

#[derive(Deserialize)]
struct WireContentPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

fn parse_chat_response(bytes: &[u8]) -> Result<TextGenerationResponse, GenerationError> {
    let response: WireChatResponse =
        serde_json::from_slice(bytes).map_err(|_| GenerationError::InvalidResponse {
            endpoint: "chat completion",
            reason: "body was not valid expected JSON",
        })?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(GenerationError::InvalidResponse {
            endpoint: "chat completion",
            reason: "choices was empty",
        })?;
    let text = match choice.message.content {
        WireContent::Text(text) => text,
        WireContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|part| part.text)
            .collect::<Vec<_>>()
            .join(""),
    };
    if text.trim().is_empty() {
        return Err(GenerationError::InvalidResponse {
            endpoint: "chat completion",
            reason: "first choice contained no text",
        });
    }
    let usage = response.usage.unwrap_or_default();
    Ok(TextGenerationResponse {
        text,
        model: None,
        finish_reason: choice.finish_reason,
        usage: TokenUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        },
    })
}

/// Exercises both untrusted provider-response decoders without network I/O.
#[cfg(feature = "fuzzing")]
pub fn fuzz_provider_responses(bytes: &[u8]) {
    if bytes.len() <= MAX_IMAGE_RESPONSE_BYTES {
        let _ = parse_chat_response(bytes);
        let _ = parse_image_response(bytes);
    }
}

#[derive(Deserialize)]
struct WireImageResponse {
    data: Vec<WireImage>,
}

#[derive(Deserialize)]
struct WireImage {
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "b64_json")]
    base64_data: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

fn parse_image_response(bytes: &[u8]) -> Result<ImageGenerationResponse, GenerationError> {
    let response: WireImageResponse =
        serde_json::from_slice(bytes).map_err(|_| GenerationError::InvalidResponse {
            endpoint: "image generation",
            reason: "body was not valid expected JSON",
        })?;
    if response.data.is_empty() {
        return Err(GenerationError::InvalidResponse {
            endpoint: "image generation",
            reason: "data was empty",
        });
    }
    if response.data.len() > usize::from(MAX_IMAGES_PER_REQUEST) {
        return Err(GenerationError::InvalidResponse {
            endpoint: "image generation",
            reason: "provider returned too many images",
        });
    }
    let mut images = Vec::with_capacity(response.data.len());
    for image in response.data {
        if image.url.as_deref().is_none_or(str::is_empty)
            && image.base64_data.as_deref().is_none_or(str::is_empty)
        {
            return Err(GenerationError::InvalidResponse {
                endpoint: "image generation",
                reason: "an image had neither url nor b64_json",
            });
        }
        if image.url.as_ref().is_some_and(|raw| {
            raw.is_empty()
                || raw.trim() != raw
                || raw.len() > 4_096
                || Url::parse(raw).map_or(true, |url| {
                    url.scheme() != "https"
                        || url.host().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                })
        }) || image
            .revised_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.chars().count() > MAX_IMAGE_PROMPT_CHARS)
        {
            return Err(GenerationError::InvalidResponse {
                endpoint: "image generation",
                reason: "an image URL or revised prompt was unsafe or unbounded",
            });
        }
        if let Some(encoded) = &image.base64_data {
            let maximum_encoded_len = MAX_DECODED_IMAGE_BYTES.div_ceil(3) * 4;
            if encoded.is_empty()
                || encoded.len() > maximum_encoded_len
                || encoded.trim() != encoded
            {
                return Err(GenerationError::InvalidResponse {
                    endpoint: "image generation",
                    reason: "base64 image data was empty or exceeded the safety limit",
                });
            }
            let decoded =
                BASE64_STANDARD
                    .decode(encoded)
                    .map_err(|_| GenerationError::InvalidResponse {
                        endpoint: "image generation",
                        reason: "b64_json was not valid standard base64",
                    })?;
            if decoded.len() > MAX_DECODED_IMAGE_BYTES || !has_supported_image_signature(&decoded) {
                return Err(GenerationError::InvalidResponse {
                    endpoint: "image generation",
                    reason: "decoded image data had an unsupported or missing signature",
                });
            }
        }
        images.push(GeneratedImage {
            url: image.url,
            base64_data: image.base64_data,
            revised_prompt: image.revised_prompt,
        });
    }
    Ok(ImageGenerationResponse { images })
}

fn has_supported_image_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
}

fn request_id(response: &Response) -> Option<String> {
    response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(128).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_content_parts_and_usage() {
        let response = parse_chat_response(
            br#"{
                "model":"local-narrator",
                "choices":[{
                    "message":{"content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]},
                    "finish_reason":"stop"
                }],
                "usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}
            }"#,
        )
        .expect("response should parse");

        assert_eq!(response.text, "hello world");
        assert_eq!(response.usage.total_tokens, Some(6));
    }

    #[test]
    fn rejects_image_without_location_or_data() {
        let error = parse_image_response(br#"{"data":[{"revised_prompt":"a knight"}]}"#)
            .expect_err("missing image payload must fail");

        assert!(matches!(
            error,
            GenerationError::InvalidResponse {
                endpoint: "image generation",
                ..
            }
        ));
    }

    #[test]
    fn rejects_non_https_provider_image_urls() {
        let error = parse_image_response(
            br#"{"data":[{"url":"http://169.254.169.254/latest/meta-data"}]}"#,
        )
        .expect_err("provider image URLs must be safe for later consumers");

        assert!(matches!(
            error,
            GenerationError::InvalidResponse {
                endpoint: "image generation",
                ..
            }
        ));
    }

    #[test]
    fn validates_base64_image_data_before_exposing_it() {
        let valid = parse_image_response(br#"{"data":[{"b64_json":"iVBORw0KGgo="}]}"#)
            .expect("PNG signature should be accepted at the adapter boundary");
        assert_eq!(valid.images.len(), 1);

        let error = parse_image_response(br#"{"data":[{"b64_json":"bm90IGFuIGltYWdl"}]}"#)
            .expect_err("arbitrary base64 text is not an image");
        assert!(matches!(error, GenerationError::InvalidResponse { .. }));
    }

    #[test]
    fn serializes_structured_chat_request() {
        let body = chat_request_body(
            "model-a",
            &TextGenerationRequest {
                messages: vec![ChatMessage::user("act")],
                response_format: TextResponseFormat::JsonObject,
                temperature: Some(0.2),
                max_output_tokens: None,
            },
            Some(512),
            None,
        );

        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["max_tokens"], 512);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn rejects_empty_or_unbounded_text_requests() {
        let empty = TextGenerationRequest {
            messages: vec![],
            response_format: TextResponseFormat::Text,
            temperature: None,
            max_output_tokens: None,
        };
        assert!(validate_text_request(&empty).is_err());

        let too_large = TextGenerationRequest {
            messages: vec![ChatMessage::user("x".repeat(MAX_TEXT_REQUEST_BYTES + 1))],
            response_format: TextResponseFormat::Text,
            temperature: None,
            max_output_tokens: None,
        };
        assert!(validate_text_request(&too_large).is_err());
    }

    #[test]
    fn fake_typed_gm_selects_only_allowlisted_actions_and_replays_facts() {
        let base = json!({
            "schema_version": 1,
            "proposal_id": "typed-gm:abcdef",
            "session_id": "local-campaign",
            "based_on_revision": 2,
            "based_on_event_sequence": 1,
            "prompt_template_id": "prompt:typed-gm-turn:v1",
            "policy_id": "policy:private-mvp:v1",
            "config_fingerprint": manchester_dnd_core::Sha256Digest::from_bytes([3; 32]),
        });
        let interpretation = json!({
            "task": "interpret_player_intent",
            "required_base": base,
            "legal_ids": { "action_ids": ["encounter-action:attack:0"] },
            "untrusted_data": {
                "player_intent": "strike the creature",
                "committed_public_facts": [{
                    "fact_id": "encounter-action:attack:0",
                    "summary": "Currently legal action: Attack Soot Wight. Use exactly action ID encounter-action:attack:0."
                }]
            }
        });
        let action: Value = serde_json::from_str(&fake_typed_gm_response(&interpretation)).unwrap();
        assert_eq!(action["type"], "action");
        assert_eq!(action["action_id"], "encounter-action:attack:0");

        let facts = json!([{"type":"outcome","outcome_id":"attack:hit"}]);
        let narration = json!({
            "task": "narrate_committed_facts",
            "required_base": interpretation["required_base"].clone(),
            "authoritative_mechanical_facts": facts,
        });
        let narrated: Value = serde_json::from_str(&fake_typed_gm_response(&narration)).unwrap();
        assert_eq!(narrated["type"], "narration");
        assert_eq!(narrated["claimed_facts"], facts);
    }

    #[tokio::test]
    async fn fake_providers_are_deterministic_bounded_and_network_free() {
        let request = TextGenerationRequest {
            messages: vec![ChatMessage::user(
                r#"{"required_output":{"proposal_schema_version":1,"proposal_id":"gm:1:test","session_id":"session:1","based_on_event_sequence":7}}"#,
            )],
            response_format: TextResponseFormat::JsonObject,
            temperature: None,
            max_output_tokens: None,
        };
        let first = FakeTextGenerator
            .generate_text(request.clone())
            .await
            .unwrap();
        let replay = FakeTextGenerator.generate_text(request).await.unwrap();
        assert_eq!(first, replay);
        let json: Value = serde_json::from_str(&first.text).unwrap();
        assert_eq!(json["session_id"], "session:1");
        assert_eq!(json["based_on_event_sequence"], 7);

        let image = FakeImageGenerator
            .generate_image(ImageGenerationRequest::one("A rain-dark arch"))
            .await
            .unwrap();
        assert_eq!(image.images.len(), 1);
        let bytes = BASE64_STANDARD
            .decode(image.images[0].base64_data.as_deref().unwrap())
            .unwrap();
        assert!(has_supported_image_signature(&bytes));
    }
}
