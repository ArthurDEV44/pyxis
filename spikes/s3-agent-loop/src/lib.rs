//! US-003 — Boucle minimale stream → outil Bash → réinjection → reboucle.
//!
//! Valide la state machine à transitions typées dans sa forme **la plus réduite**
//! (cf. docs/ROADMAP.md Phase 0) : `enum Transition` exhaustif, dispatch d'un seul
//! outil (`bash`) sous `tokio::time::timeout`, réinjection du résultat, reboucle
//! jusqu'à `end_turn`. Les transitions Compact/Recover de l'archi complète
//! (US-006/US-008) sont hors scope ici, par décision de la roadmap.
//!
//! `Provider` est un trait injectable (cf. invariant « deps injectables ») : la
//! boucle est testable sans API réelle via `ScriptedProvider`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use futures_util::stream::BoxStream;
use std::collections::HashMap;
use std::time::Duration;

pub use spike_canon::{AdapterError, StopReason, StreamEvent};

// ───────────────────────────── Provider injectable ──────────────────────────

/// Source de `StreamEvent` (réelle ou scriptée). Object-safe : la boucle prend
/// `&dyn Provider`.
pub trait Provider: Send + Sync {
    fn stream(
        &self,
        messages: Vec<serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamEvent, AdapterError>>;
}

/// Provider scripté pour les tests : rend une liste d'events figée par tour.
pub struct ScriptedProvider {
    turns: std::sync::Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
}

impl ScriptedProvider {
    pub fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            turns: std::sync::Mutex::new(turns.into()),
        }
    }
}

impl Provider for ScriptedProvider {
    fn stream(
        &self,
        _messages: Vec<serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamEvent, AdapterError>> {
        use futures_util::StreamExt;
        let turn = self
            .turns
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
            .unwrap_or_default();
        async_stream::stream! {
            for e in turn {
                yield Ok(e);
            }
        }
        .boxed()
    }
}

/// Provider live OpenAI-compat (Ollama / OpenAI), via `spike_canon`.
pub struct LiveProvider {
    pub base: String,
    pub api_key: Option<String>,
    pub model: String,
    pub tools: Option<serde_json::Value>,
}

impl Provider for LiveProvider {
    fn stream(
        &self,
        messages: Vec<serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamEvent, AdapterError>> {
        use futures_util::StreamExt;
        let base = self.base.clone();
        let key = self.api_key.clone();
        let body = spike_canon::build_body(
            &self.model,
            serde_json::Value::Array(messages),
            self.tools.clone(),
        );
        async_stream::stream! {
            match spike_canon::stream_chat(&base, key.as_deref(), body).await {
                Ok(mut s) => {
                    while let Some(ev) = s.next().await {
                        yield ev;
                    }
                }
                Err(e) => yield Err(e),
            }
        }
        .boxed()
    }
}

// ───────────────────────────── Accumulateur de tour ─────────────────────────

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

struct PartialCall {
    name: String,
    args: String,
}

/// Accumule les `StreamEvent` d'un tour en un état décisionnel.
#[derive(Default)]
pub struct Accumulator {
    pub text: String,
    pub reasoning: String,
    pub stop: Option<StopReason>,
    open: HashMap<String, PartialCall>,
    order: Vec<String>,
}

impl Accumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, ev: StreamEvent) {
        match ev {
            StreamEvent::TextDelta { text } => self.text.push_str(&text),
            StreamEvent::ReasoningDelta { text } => self.reasoning.push_str(&text),
            StreamEvent::ToolCallStart { id, name } => {
                self.open.insert(
                    id.clone(),
                    PartialCall {
                        name,
                        args: String::new(),
                    },
                );
                self.order.push(id);
            }
            StreamEvent::ToolCallDelta { id, args_json } => {
                if let Some(p) = self.open.get_mut(&id) {
                    p.args.push_str(&args_json);
                } else {
                    self.open.insert(
                        id.clone(),
                        PartialCall {
                            name: String::new(),
                            args: args_json,
                        },
                    );
                    self.order.push(id);
                }
            }
            StreamEvent::ToolCallEnd { .. } | StreamEvent::Usage { .. } => {}
            StreamEvent::Done { stop } => self.stop = Some(stop),
        }
    }

    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.order
            .iter()
            .filter_map(|id| {
                self.open.get(id).map(|p| ToolCall {
                    id: id.clone(),
                    name: p.name.clone(),
                    args_json: p.args.clone(),
                })
            })
            .collect()
    }
}

// ──────────────────────────── State machine typée ───────────────────────────

/// Transition exhaustive (forme réduite Phase 0). Le `match` du driver force le
/// traitement de tous les cas → contrôle de flux vérifié à la compilation.
#[derive(Debug)]
pub enum Transition {
    /// Le modèle a fini sans tool_use → rendre la main.
    EndTurn,
    /// Le modèle demande des outils → exécuter puis reboucler.
    RunTools(Vec<ToolCall>),
    /// Plafond de tours / max_tokens.
    Exhausted(String),
    /// Erreur fatale → propager.
    Fail(String),
}

/// Pur, sans I/O → testable unitairement (nœud de la testabilité headless).
pub fn decide_transition(acc: &Accumulator) -> Transition {
    let calls = acc.tool_calls();
    if !calls.is_empty() && matches!(acc.stop, Some(StopReason::ToolUse)) {
        return Transition::RunTools(calls);
    }
    match acc.stop {
        Some(StopReason::EndTurn) | Some(StopReason::StopSequence) | None => Transition::EndTurn,
        Some(StopReason::MaxTokens) => Transition::Exhausted("max_tokens".to_string()),
        Some(StopReason::Refusal) => Transition::Fail("refusal".to_string()),
        // ToolUse annoncé mais aucun call assemblé → fail-closed vers EndTurn.
        Some(StopReason::ToolUse) => Transition::EndTurn,
    }
}

// ─────────────────────────────── Outil Bash ─────────────────────────────────

#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub name: String,
    pub args: String,
    pub output: String,
    /// Toute sortie d'outil est untrusted par défaut (taint = US-013).
    pub untrusted: bool,
    pub timed_out: bool,
}

/// Exécute `bash -c <cmd>` sous timeout. Un outil qui pend ne fige pas la boucle :
/// le timeout reprend la main (`kill_on_drop` tue le process orphelin).
pub async fn exec_bash(id_name: &str, args_json: &str, timeout: Duration) -> ToolInvocation {
    let cmd = serde_json::from_str::<serde_json::Value>(args_json)
        .ok()
        .and_then(|v| {
            v.get("cmd")
                .or_else(|| v.get("command"))
                .and_then(|s| s.as_str().map(str::to_string))
        })
        .unwrap_or_default();

    if cmd.is_empty() {
        return ToolInvocation {
            name: id_name.to_string(),
            args: args_json.to_string(),
            output: "erreur: argument `cmd` manquant ou args non-JSON".to_string(),
            untrusted: true,
            timed_out: false,
        };
    }

    let fut = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .kill_on_drop(true)
        .output();

    let (output, timed_out) = match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(o)) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str(&err);
            }
            (s.trim().to_string(), false)
        }
        Ok(Err(e)) => (format!("erreur exec: {e}"), false),
        Err(_) => (
            "[timeout] outil interrompu — la boucle reprend la main".to_string(),
            true,
        ),
    };

    ToolInvocation {
        name: id_name.to_string(),
        args: cmd,
        output,
        untrusted: true,
        timed_out,
    }
}

// ──────────────────────────────── Driver ────────────────────────────────────

#[derive(Debug)]
pub enum EndState {
    EndTurn,
    Exhausted(String),
    Fail(String),
}

#[derive(Debug)]
pub struct RunOutcome {
    pub final_text: String,
    pub turns: usize,
    pub invocations: Vec<ToolInvocation>,
    pub ended: EndState,
}

/// La boucle complète : stream → décision → (outil → réinjection → reboucle) | fin.
pub async fn run_agent(
    provider: &dyn Provider,
    system: Option<&str>,
    user: &str,
    max_turns: usize,
    tool_timeout: Duration,
) -> RunOutcome {
    use futures_util::StreamExt;

    let mut messages: Vec<serde_json::Value> = Vec::new();
    if let Some(sys) = system {
        messages.push(serde_json::json!({ "role": "system", "content": sys }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": user }));

    let mut invocations = Vec::new();
    let mut turns = 0usize;

    loop {
        if turns >= max_turns {
            return RunOutcome {
                final_text: String::new(),
                turns,
                invocations,
                ended: EndState::Exhausted(format!("max_turns={max_turns}")),
            };
        }
        turns += 1;

        // transcript-before-response serait ici (US-006/US-009) — hors scope spike.
        let mut acc = Accumulator::new();
        let mut stream = provider.stream(messages.clone());
        let mut stream_err = None;
        while let Some(ev) = stream.next().await {
            match ev {
                Ok(se) => acc.push(se),
                Err(e) => {
                    stream_err = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(e) = stream_err {
            return RunOutcome {
                final_text: acc.text,
                turns,
                invocations,
                ended: EndState::Fail(e),
            };
        }

        match decide_transition(&acc) {
            Transition::EndTurn => {
                return RunOutcome {
                    final_text: acc.text,
                    turns,
                    invocations,
                    ended: EndState::EndTurn,
                };
            }
            Transition::Exhausted(why) => {
                return RunOutcome {
                    final_text: acc.text,
                    turns,
                    invocations,
                    ended: EndState::Exhausted(why),
                };
            }
            Transition::Fail(why) => {
                return RunOutcome {
                    final_text: acc.text,
                    turns,
                    invocations,
                    ended: EndState::Fail(why),
                };
            }
            Transition::RunTools(calls) => {
                // message assistant (avec tool_calls) ajouté au transcript
                let tool_calls_json: Vec<serde_json::Value> = calls
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.args_json },
                        })
                    })
                    .collect();
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": acc.text,
                    "tool_calls": tool_calls_json,
                }));

                // exécution + réinjection de chaque résultat
                for call in calls {
                    let inv = exec_bash(&call.name, &call.args_json, tool_timeout).await;
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": inv.output,
                    }));
                    invocations.push(inv);
                }
                // reboucle : le modèle voit les résultats
            }
        }
    }
}

// ─────────────────────────────────── Tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_turn(id: &str, cmd: &str) -> Vec<StreamEvent> {
        let args = serde_json::json!({ "cmd": cmd }).to_string();
        vec![
            StreamEvent::ToolCallStart {
                id: id.into(),
                name: "bash".into(),
            },
            StreamEvent::ToolCallDelta {
                id: id.into(),
                args_json: args,
            },
            StreamEvent::ToolCallEnd { id: id.into() },
            StreamEvent::Done {
                stop: StopReason::ToolUse,
            },
        ]
    }

    fn text_turn(t: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta { text: t.into() },
            StreamEvent::Done {
                stop: StopReason::EndTurn,
            },
        ]
    }

    // AC1 + AC3 : tool_use → exécution → réinjection → reboucle → end_turn propre.
    #[tokio::test]
    async fn loop_runs_tool_then_ends() {
        let provider = ScriptedProvider::new(vec![
            bash_turn("call_1", "echo bonjour-numen"),
            text_turn("Voilà, c'est fait."),
        ]);
        let out = run_agent(&provider, None, "fais un echo", 5, Duration::from_secs(5)).await;

        assert_eq!(out.turns, 2, "doit reboucler exactement une fois");
        assert!(
            matches!(out.ended, EndState::EndTurn),
            "fin propre attendue"
        );
        assert_eq!(out.invocations.len(), 1);
        assert_eq!(out.invocations[0].output, "bonjour-numen");
        assert!(
            out.invocations[0].untrusted,
            "sortie outil = untrusted par défaut"
        );
        assert_eq!(out.final_text, "Voilà, c'est fait.");
    }

    // AC2 : un outil qui dépasse le timeout ne fige pas la boucle.
    #[tokio::test]
    async fn tool_timeout_does_not_freeze_loop() {
        let provider = ScriptedProvider::new(vec![
            bash_turn("call_1", "sleep 5"),
            text_turn("repris la main."),
        ]);
        let out = run_agent(&provider, None, "dors", 5, Duration::from_millis(200)).await;

        assert!(out.invocations[0].timed_out, "le timeout doit être signalé");
        assert!(
            matches!(out.ended, EndState::EndTurn),
            "la boucle continue et se ferme"
        );
        assert_eq!(out.turns, 2);
    }

    #[test]
    fn decide_transition_is_exhaustive_and_pure() {
        let mut acc = Accumulator::new();
        acc.push(StreamEvent::Done {
            stop: StopReason::EndTurn,
        });
        assert!(matches!(decide_transition(&acc), Transition::EndTurn));

        let mut acc = Accumulator::new();
        acc.push(StreamEvent::ToolCallStart {
            id: "x".into(),
            name: "bash".into(),
        });
        acc.push(StreamEvent::Done {
            stop: StopReason::ToolUse,
        });
        assert!(matches!(decide_transition(&acc), Transition::RunTools(_)));

        let mut acc = Accumulator::new();
        acc.push(StreamEvent::Done {
            stop: StopReason::MaxTokens,
        });
        assert!(matches!(decide_transition(&acc), Transition::Exhausted(_)));
    }

    #[tokio::test]
    async fn missing_cmd_is_handled_without_panic() {
        let inv = exec_bash("bash", "not json", Duration::from_secs(1)).await;
        assert!(inv.output.contains("manquant"));
        assert!(!inv.timed_out);
    }
}
