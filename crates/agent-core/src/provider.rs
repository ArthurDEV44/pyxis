//! Contrat `Provider` + vocabulaire de streaming canonique.
//!
//! ⚠️ Réconciliation Cargo vs docs : `StreamEvent` et le trait `Provider` sont
//! conceptuellement « couche provider » (PROVIDERS §2), mais l'**invariant 1**
//! (ARCHITECTURE §2 : `agent-core` ne dépend PAS d'`agent-provider`) impose que
//! le **contrat** vive ici, dans le crate des types canoniques. `agent-provider`
//! (futur) implémentera ce trait et dépendra d'`agent-core`. Le cœur consomme
//! `dyn Provider` injecté — il ne connaît aucun adapter concret.

use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::message::{Message, ToolCallId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenAiChat,
    /// Abonnement ChatGPT, Responses API sur le backend ChatGPT (ADR-10) — cible
    /// du MVP. Les autres providers s'ajouteront ensuite (pas Ollama : retiré du
    /// scope, jugé trop instable).
    OpenAiChatGpt,
    OpenAiResponses,
    Gemini,
    OpenRouter,
}

/// Le seul vocabulaire de streaming que le cœur connaît (PROVIDERS §2). Tout
/// adapter doit produire CETTE séquence. À `ToolCallEnd`, la concaténation des
/// `ToolCallDelta.args_json` d'un même id DOIT être un JSON valide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStart {
        id: ToolCallId,
        name: String,
    },
    ToolCallDelta {
        id: ToolCallId,
        args_json: String,
    },
    ToolCallEnd {
        id: ToolCallId,
    },
    Usage {
        usage: TokenUsage,
    },
    Done {
        stop: StopReason,
    },
    /// Reasoning item chiffré (US-031, replay isolé) : émis par l'adapter UNIQUEMENT
    /// si `reasoning_replay` est actif. Capturé par l'`Accumulator`.
    EncryptedReasoning {
        id: String,
        encrypted_content: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
}

impl TokenUsage {
    pub fn total(&self) -> u32 {
        self.input.saturating_add(self.output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub vision: bool,
    pub tools: bool,
    pub prompt_caching: bool,
    pub reasoning: bool,
    pub server_side_state: bool,
    pub max_context: u32,
    #[serde(default)]
    pub limits: CapabilityLimits,
    #[serde(default)]
    pub tool_calling: ToolCallingCapabilities,
    #[serde(default)]
    pub reasoning_options: ReasoningCapabilities,
    #[serde(default)]
    pub cache: CacheCapabilities,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            vision: false,
            tools: false,
            prompt_caching: false,
            reasoning: false,
            server_side_state: false,
            max_context: 0,
            limits: CapabilityLimits::default(),
            tool_calling: ToolCallingCapabilities::default(),
            reasoning_options: ReasoningCapabilities::default(),
            cache: CacheCapabilities::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapabilityLimits {
    pub max_images_per_request: Option<u32>,
    pub max_tool_schema_bytes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolCallingCapabilities {
    pub parallel_tool_calls: bool,
    pub strict_json_schema: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReasoningCapabilities {
    pub encrypted_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CacheCapabilities {
    pub prompt_cache_key: bool,
}

/// Définition d'outil exposée au modèle (JSON Schema d'entrée).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl ToolSpec {
    pub fn validate(&self) -> Result<(), ToolSpecValidationError> {
        if self.name.trim().is_empty() {
            return Err(ToolSpecValidationError::EmptyName);
        }
        if !self
            .input_schema
            .as_object()
            .and_then(|schema| schema.get("type"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind == "object")
        {
            return Err(ToolSpecValidationError::SchemaMustBeObject {
                tool: self.name.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolSpecValidationError {
    #[error("tool name is empty")]
    EmptyName,
    #[error("tool {tool} input_schema must be a JSON schema object")]
    SchemaMustBeObject { tool: String },
}

/// Requête canonique (ce que `ctx.request()` produit). Transcript client-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: u32,
}

impl CanonicalRequest {
    pub fn validate(&self) -> Result<(), CanonicalRequestValidationError> {
        if self.model.trim().is_empty() {
            return Err(CanonicalRequestValidationError::EmptyModel);
        }
        if self.max_output_tokens == 0 {
            return Err(CanonicalRequestValidationError::ZeroMaxOutputTokens);
        }
        for (index, message) in self.messages.iter().enumerate() {
            message.validate().map_err(|source| {
                CanonicalRequestValidationError::InvalidMessage {
                    index,
                    detail: source.to_string(),
                }
            })?;
        }
        for tool in &self.tools {
            tool.validate()
                .map_err(CanonicalRequestValidationError::InvalidTool)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalRequestValidationError {
    #[error("model is empty")]
    EmptyModel,
    #[error("max_output_tokens must be greater than zero")]
    ZeroMaxOutputTokens,
    #[error("message {index} is invalid: {detail}")]
    InvalidMessage { index: usize, detail: String },
    #[error("tool spec is invalid: {0}")]
    InvalidTool(#[from] ToolSpecValidationError),
}

/// Réponse non-stream (utilitaire : titres, résumés de compaction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub content: Vec<crate::message::ContentBlock>,
    pub usage: TokenUsage,
    pub stop: StopReason,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("transport: {0}")]
    Transport(String),
    /// Erreur HTTP non-2xx. `retry_after_ms` (US-023) porte le délai serveur
    /// parsé (`Retry-After` / `retry-after-ms`) quand présent : la boucle l'honore
    /// via `max(backoff, retry_after)`. `None` = pas d'en-tête → backoff seul.
    #[error("http {status}: {message}")]
    Http {
        status: u16,
        message: String,
        retry_after_ms: Option<u64>,
    },
    #[error("décodage: {0}")]
    Decode(String),
    #[error("flux interrompu: {0}")]
    Stream(String),
    /// Erreur de CONTEXTE (PTL / 413). N'est PAS une classe transitoire : elle
    /// alimente le withholding (ARCHITECTURE §3.4), pas le backoff.
    #[error("contexte trop long (PTL/413)")]
    ContextLengthExceeded,
}

impl ProviderError {
    /// Vrai si l'erreur est une erreur de **contexte** (PTL/413/max-tokens
    /// d'entrée) → alimente `PendingError`/withholding, jamais le retry.
    pub fn is_context_error(&self) -> bool {
        matches!(
            self,
            ProviderError::ContextLengthExceeded | ProviderError::Http { status: 413, .. }
        )
    }
}

/// Taxonomie d'erreurs canonique (ADR-9). Nommée `ErrorClass` partout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Retryable,
    RateLimited,
    Overloaded(u16),
    Auth(AuthError),
    InvalidRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthError {
    Expired,
    ThirdPartyBlocked,
    Invalid,
}

/// Implémenté par chaque adapter (dans `agent-provider`). Object-safe via
/// `async-trait` → consommé en `dyn Provider`.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn capabilities(&self) -> &Capabilities;

    /// Chemin chaud : flux d'événements canoniques.
    async fn stream(
        &self,
        req: CanonicalRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>;

    /// Non-stream (utilisé par la compaction pour produire un résumé).
    async fn complete(&self, req: CanonicalRequest) -> Result<CanonicalResponse, ProviderError>;

    /// Classifie une erreur transport/HTTP en `ErrorClass` (source de vérité du
    /// retry). Les erreurs de contexte ne passent PAS par ici (cf. withholding).
    fn classify_error(&self, err: &ProviderError) -> ErrorClass;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Message, Role};

    #[test]
    fn context_error_detection() {
        assert!(ProviderError::ContextLengthExceeded.is_context_error());
        assert!(
            ProviderError::Http {
                status: 413,
                message: "too long".into(),
                retry_after_ms: None,
            }
            .is_context_error()
        );
        assert!(
            !ProviderError::Http {
                status: 529,
                message: "overloaded".into(),
                retry_after_ms: None,
            }
            .is_context_error()
        );
        assert!(!ProviderError::Transport("reset".into()).is_context_error());
    }

    #[test]
    fn canonical_request_validation_rejects_invalid_message_and_tool_schema() {
        let invalid_message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "out".into(),
                untrusted: true,
                is_error: false,
            }],
        };
        let req = CanonicalRequest {
            model: "gpt".into(),
            system: None,
            messages: vec![invalid_message],
            tools: vec![],
            max_output_tokens: 100,
        };
        assert!(matches!(
            req.validate(),
            Err(CanonicalRequestValidationError::InvalidMessage { .. })
        ));

        let req = CanonicalRequest {
            model: "gpt".into(),
            system: None,
            messages: vec![Message::user("ok")],
            tools: vec![ToolSpec {
                name: "bad".into(),
                description: String::new(),
                input_schema: serde_json::json!({ "type": "string" }),
            }],
            max_output_tokens: 100,
        };
        assert!(matches!(
            req.validate(),
            Err(CanonicalRequestValidationError::InvalidTool(
                ToolSpecValidationError::SchemaMustBeObject { .. }
            ))
        ));
    }
}
