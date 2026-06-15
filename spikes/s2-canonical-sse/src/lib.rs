//! US-002 — Couche canonique : un flux SSE provider → `StreamEvent` typés.
//!
//! Prouve que la couche maison (`reqwest` + `eventsource-stream`, sans SDK) tient.
//! Le vocabulaire `StreamEvent` est celui figé dans `docs/PROVIDERS.md §2`
//! (Anthropic-like). L'adapter ici cible le wire format **OpenAI Chat Completions**
//! (le même que sert Ollama en mode `/v1/chat/completions`), réutilisé par S1 et S3.
//!
//! Invariant clé : à `ToolCallEnd`, la concaténation des `ToolCallDelta.args_json`
//! d'un même id forme un JSON complet et valide (cf. `PROVIDERS.md §2`).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use futures_util::stream::BoxStream;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

// ───────────────────────────── Vocabulaire canonique ─────────────────────────

/// Le seul vocabulaire de streaming que le cœur connaît (cf. `PROVIDERS.md §2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, args_json: String },
    ToolCallEnd { id: String },
    Usage { usage: TokenUsage },
    Done { stop: StopReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
}

impl StopReason {
    fn from_finish(s: &str) -> Self {
        match s {
            "stop" => StopReason::EndTurn,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::Refusal,
            _ => StopReason::EndTurn,
        }
    }
}

/// Erreur d'adapter classifiée — jamais un panic (US-002 AC2/AC3).
#[derive(Debug, Clone)]
pub enum AdapterError {
    /// Échec transport/connexion (Ollama éteint, DNS, refus).
    Transport(String),
    /// Réponse HTTP non-2xx (porte le code pour classification ultérieure).
    Http { status: u16, body: String },
    /// Chunk JSON malformé — ignoré ou remonté sans crasher le parseur.
    Json(String),
    /// Flux coupé en milieu de message.
    Stream(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Transport(m) => write!(f, "transport: {m}"),
            AdapterError::Http { status, body } => write!(f, "http {status}: {body}"),
            AdapterError::Json(m) => write!(f, "chunk JSON malformé: {m}"),
            AdapterError::Stream(m) => write!(f, "flux interrompu: {m}"),
        }
    }
}
impl std::error::Error for AdapterError {}

// ───────────────────────── Adapter OpenAI Chat (stateful) ────────────────────

/// Désérialisation du wire format OpenAI Chat Completions (chunk de stream).
#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    // Variantes de reasoning selon provider (OpenAI o-series n'expose rien ;
    // certains modèles via Ollama exposent `reasoning_content`).
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawToolCall>,
}

#[derive(Deserialize)]
struct RawToolCall {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<RawFn>,
}

#[derive(Deserialize)]
struct RawFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

/// Adapter à état : réassemble les tool calls fragmentés par `index` et garantit
/// l'invariant `args_json` complet à `ToolCallEnd`.
#[derive(Default)]
pub struct OpenAiChatAdapter {
    index_to_id: HashMap<u32, String>,
    started: Vec<String>,
    ended: HashSet<String>,
}

impl OpenAiChatAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn id_for(&mut self, tc: &RawToolCall) -> String {
        if let Some(id) = &tc.id {
            self.index_to_id.insert(tc.index, id.clone());
            id.clone()
        } else {
            self.index_to_id
                .get(&tc.index)
                .cloned()
                .unwrap_or_else(|| format!("call_{}", tc.index))
        }
    }

    /// Traduit un `data:` SSE (un chunk JSON) en 0..n `StreamEvent` canoniques.
    /// `[DONE]` ne produit rien : `Done` est émis sur `finish_reason`.
    pub fn ingest(&mut self, data: &str) -> Result<Vec<StreamEvent>, AdapterError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let chunk: Chunk =
            serde_json::from_str(data).map_err(|e| AdapterError::Json(e.to_string()))?;

        let mut out = Vec::new();
        for choice in &chunk.choices {
            if let Some(t) = &choice.delta.content
                && !t.is_empty()
            {
                out.push(StreamEvent::TextDelta { text: t.clone() });
            }
            if let Some(r) = choice
                .delta
                .reasoning
                .as_ref()
                .or(choice.delta.reasoning_content.as_ref())
                && !r.is_empty()
            {
                out.push(StreamEvent::ReasoningDelta { text: r.clone() });
            }
            for tc in &choice.delta.tool_calls {
                let id = self.id_for(tc);
                if let Some(f) = &tc.function {
                    if let Some(name) = &f.name
                        && !self.started.contains(&id)
                    {
                        self.started.push(id.clone());
                        out.push(StreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                    }
                    if let Some(args) = &f.arguments
                        && !args.is_empty()
                    {
                        out.push(StreamEvent::ToolCallDelta {
                            id: id.clone(),
                            args_json: args.clone(),
                        });
                    }
                }
            }
            if let Some(reason) = &choice.finish_reason {
                if reason == "tool_calls" {
                    let to_close: Vec<String> = self
                        .started
                        .iter()
                        .filter(|id| !self.ended.contains(*id))
                        .cloned()
                        .collect();
                    for id in to_close {
                        self.ended.insert(id.clone());
                        out.push(StreamEvent::ToolCallEnd { id });
                    }
                }
                out.push(StreamEvent::Done {
                    stop: StopReason::from_finish(reason),
                });
            }
        }

        if let Some(u) = &chunk.usage {
            out.push(StreamEvent::Usage {
                usage: TokenUsage {
                    input: u.prompt_tokens,
                    output: u.completion_tokens,
                    total: u.total_tokens,
                },
            });
        }
        Ok(out)
    }
}

// ───────────────────────────── Streaming live (reqwest) ──────────────────────

/// Construit le corps de requête OpenAI-compat (stream + usage).
pub fn build_body(
    model: &str,
    messages: serde_json::Value,
    tools: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(tools) = tools {
        body["tools"] = tools;
    }
    body
}

/// Ouvre un flux `StreamEvent` depuis un endpoint OpenAI-compat `/chat/completions`.
/// `base_url` = ex. `http://localhost:11434/v1` (Ollama) ou `https://api.openai.com/v1`.
pub async fn stream_chat(
    base_url: &str,
    api_key: Option<&str>,
    body: serde_json::Value,
) -> Result<BoxStream<'static, Result<StreamEvent, AdapterError>>, AdapterError> {
    use eventsource_stream::Eventsource;
    use futures_util::StreamExt;

    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{base_url}/chat/completions"))
        .json(&body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| AdapterError::Transport(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AdapterError::Http {
            status: status.as_u16(),
            body,
        });
    }

    let mut adapter = OpenAiChatAdapter::new();
    let mut es = resp.bytes_stream().eventsource();

    let s = async_stream::stream! {
        while let Some(ev) = es.next().await {
            match ev {
                Ok(event) => match adapter.ingest(&event.data) {
                    Ok(events) => {
                        for e in events {
                            yield Ok(e);
                        }
                    }
                    // Chunk malformé : erreur typée, le parseur ne crashe pas (AC3).
                    Err(e) => yield Err(e),
                },
                // Flux coupé en milieu de message : classifié, pas de panic (AC2).
                Err(e) => {
                    yield Err(AdapterError::Stream(e.to_string()));
                    return;
                }
            }
        }
    };

    Ok(s.boxed())
}

// ─────────────────────────────────── Tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_deltas_map_to_textdelta() {
        let mut a = OpenAiChatAdapter::new();
        let ev = a
            .ingest(r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#)
            .unwrap();
        assert_eq!(
            ev,
            vec![StreamEvent::TextDelta {
                text: "Hello".into()
            }]
        );
    }

    #[test]
    fn finish_stop_emits_done_endturn() {
        let mut a = OpenAiChatAdapter::new();
        let ev = a
            .ingest(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(
            ev,
            vec![StreamEvent::Done {
                stop: StopReason::EndTurn
            }]
        );
    }

    #[test]
    fn usage_chunk_maps_to_usage() {
        let mut a = OpenAiChatAdapter::new();
        let ev = a
            .ingest(r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}}"#)
            .unwrap();
        assert_eq!(
            ev,
            vec![StreamEvent::Usage {
                usage: TokenUsage {
                    input: 12,
                    output: 8,
                    total: 20
                }
            }]
        );
    }

    #[test]
    fn tool_call_fragmented_reassembles_to_valid_json_at_end() {
        let mut a = OpenAiChatAdapter::new();
        // 1er fragment : id + name + début d'args
        let _ = a
            .ingest(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":"{\"cmd\":\""}}]},"finish_reason":null}]}"#,
            )
            .unwrap();
        // 2e fragment : suite d'args, SANS id (résolu par index)
        let _ = a
            .ingest(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"echo hi\"}"}}]},"finish_reason":null}]}"#,
            )
            .unwrap();
        // clôture
        let end = a
            .ingest(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
            .unwrap();

        // On rejoue tout pour reconstituer le flux complet et vérifier l'invariant.
        let mut a2 = OpenAiChatAdapter::new();
        let mut all = Vec::new();
        for c in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":"{\"cmd\":\""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"echo hi\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ] {
            all.extend(a2.ingest(c).unwrap());
        }

        // Start présent, End présent (issu du dernier chunk), Done = ToolUse.
        assert!(all.contains(&StreamEvent::ToolCallStart {
            id: "call_1".into(),
            name: "bash".into()
        }));
        assert!(end.contains(&StreamEvent::ToolCallEnd {
            id: "call_1".into()
        }));
        assert!(all.contains(&StreamEvent::Done {
            stop: StopReason::ToolUse
        }));

        // Invariant : args_json concaténé = JSON valide.
        let args: String = all
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallDelta { id, args_json } if id == "call_1" => {
                    Some(args_json.clone())
                }
                _ => None,
            })
            .collect();
        let parsed: serde_json::Value = serde_json::from_str(&args).expect("args_json valide");
        assert_eq!(parsed["cmd"], "echo hi");
    }

    #[test]
    fn malformed_chunk_yields_typed_error_not_panic() {
        let mut a = OpenAiChatAdapter::new();
        let err = a.ingest("{ this is not json ]").unwrap_err();
        assert!(
            matches!(err, AdapterError::Json(_)),
            "attendu Json, eu {err:?}"
        );
    }

    #[test]
    fn done_sentinel_is_noop() {
        let mut a = OpenAiChatAdapter::new();
        assert_eq!(a.ingest("[DONE]").unwrap(), Vec::new());
        assert_eq!(a.ingest("").unwrap(), Vec::new());
    }
}
