//! État de rendu côté client (US-019). `AppState` consomme les `AgentEvent` du
//! cœur (jamais d'ANSI) et les range en `Block`s typés ; le rendu (`render.rs`)
//! décide seul de la présentation. La gestion clavier renvoie une `InputAction`
//! que la boucle agent-cli interprète (soumission, permission, quit, scroll).

use agent_core::AgentEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Un élément du transcript. Le rendu choisit poids/teinte ; aucune couleur ici.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Tour utilisateur.
    User(String),
    /// Tour assistant (texte streamé). `streaming` = curseur live actif.
    Assistant { text: String, streaming: bool },
    /// Raisonnement du modèle (rendu en sourdine).
    Reasoning(String),
    /// Un outil va s'exécuter.
    ToolCall { name: String, summary: String },
    /// Résultat d'un outil (taint + erreur portés pour le rendu).
    ToolResult {
        content: String,
        untrusted: bool,
        is_error: bool,
    },
    /// Information système discrète (compaction, budget…).
    Notice(String),
    /// Erreur remontée par le cœur.
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Thinking,
}

/// Une ligne d'un aperçu de mutation (diff) présenté dans le dialog de permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Remove,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// Demande de confirmation présentée à l'utilisateur (générique : la boucle
/// agent-cli la construit depuis la `PermissionRequest` d'`agent-tools`, en
/// pré-rendant un diff pour les éditions).
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionPrompt {
    pub title: String,
    pub reason: String,
    pub detail: Vec<DiffLine>,
}

#[derive(Clone)]
pub struct AppState {
    pub blocks: Vec<Block>,
    pub input: String,
    pub status: Status,
    pub pending: Option<PermissionPrompt>,
    pub truecolor: bool,
    /// Décalage de scroll vers le HAUT (0 = collé en bas, suit le live).
    pub scroll: u16,
    pub model: String,
    pub should_quit: bool,
}

/// Action déduite d'une touche, interprétée par la boucle agent-cli.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    None,
    Submit(String),
    Quit,
    Permission(bool),
    ScrollUp,
    ScrollDown,
}

impl AppState {
    pub fn new(model: impl Into<String>, truecolor: bool) -> Self {
        Self {
            blocks: Vec::new(),
            input: String::new(),
            status: Status::Idle,
            pending: None,
            truecolor,
            scroll: 0,
            model: model.into(),
            should_quit: false,
        }
    }

    /// Range un `AgentEvent` du cœur dans le transcript.
    pub fn apply(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::Text(t) => {
                self.status = Status::Thinking;
                match self.blocks.last_mut() {
                    Some(Block::Assistant {
                        text,
                        streaming: true,
                    }) => text.push_str(t),
                    _ => self.blocks.push(Block::Assistant {
                        text: t.clone(),
                        streaming: true,
                    }),
                }
            }
            AgentEvent::Reasoning(t) => {
                self.status = Status::Thinking;
                match self.blocks.last_mut() {
                    Some(Block::Reasoning(r)) => r.push_str(t),
                    _ => self.blocks.push(Block::Reasoning(t.clone())),
                }
            }
            AgentEvent::ToolCall(view) => {
                self.finalize_streaming();
                self.blocks.push(Block::ToolCall {
                    name: view.name.clone(),
                    summary: summarize(&view.input),
                });
            }
            AgentEvent::ToolResult(view) => {
                self.blocks.push(Block::ToolResult {
                    content: view.content.clone(),
                    untrusted: view.untrusted,
                    is_error: view.is_error,
                });
            }
            AgentEvent::Compacted(_) => self.blocks.push(Block::Notice("contexte compacté".into())),
            AgentEvent::PermissionAsk(req) => self
                .blocks
                .push(Block::Notice(format!("permission : {}", req.tool))),
            AgentEvent::EndTurn => {
                self.finalize_streaming();
                self.status = Status::Idle;
            }
            AgentEvent::Exhausted(reason) => {
                self.finalize_streaming();
                self.blocks
                    .push(Block::Notice(format!("arrêt : {reason:?}")));
                self.status = Status::Idle;
            }
            AgentEvent::Error(e) => {
                self.finalize_streaming();
                self.blocks.push(Block::Error(e.to_string()));
                self.status = Status::Idle;
            }
        }
    }

    /// Pousse le tour utilisateur (appelé à la soumission).
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::User(text.into()));
        self.status = Status::Thinking;
        self.scroll = 0;
    }

    fn finalize_streaming(&mut self) {
        if let Some(Block::Assistant { streaming, .. }) = self.blocks.last_mut() {
            *streaming = false;
        }
    }

    /// Gestion clavier. En attente de permission, seules o/n/Enter/Esc comptent.
    pub fn on_key(&mut self, key: KeyEvent) -> InputAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return InputAction::Quit;
        }

        if self.pending.is_some() {
            return match key.code {
                KeyCode::Char('o') | KeyCode::Char('y') | KeyCode::Enter => {
                    self.pending = None;
                    InputAction::Permission(true)
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.pending = None;
                    InputAction::Permission(false)
                }
                _ => InputAction::None,
            };
        }

        match key.code {
            KeyCode::Enter => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    InputAction::None
                } else {
                    self.input.clear();
                    InputAction::Submit(text)
                }
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                InputAction::None
            }
            KeyCode::Backspace => {
                self.input.pop();
                InputAction::None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(5);
                InputAction::ScrollUp
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(5);
                InputAction::ScrollDown
            }
            _ => InputAction::None,
        }
    }
}

/// Résumé court d'un input d'outil pour l'affichage (bash → la commande, etc.).
fn summarize(input: &serde_json::Value) -> String {
    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
        return cmd.to_string();
    }
    if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
        return path.to_string();
    }
    if let Some(pat) = input.get("pattern").and_then(|v| v.as_str()) {
        return pat.to_string();
    }
    let s = input.to_string();
    if s.len() > 80 {
        format!("{}…", &s[..80])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::event::{ToolCallView, ToolResultView};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn streamed_text_accumulates_into_one_assistant_block() {
        let mut s = AppState::new("gpt-5", false);
        s.apply(&AgentEvent::Text("Bon".into()));
        s.apply(&AgentEvent::Text("jour".into()));
        assert_eq!(s.blocks.len(), 1);
        assert_eq!(
            s.blocks[0],
            Block::Assistant {
                text: "Bonjour".into(),
                streaming: true
            }
        );
        s.apply(&AgentEvent::EndTurn);
        assert!(matches!(
            s.blocks[0],
            Block::Assistant {
                streaming: false,
                ..
            }
        ));
        assert_eq!(s.status, Status::Idle);
    }

    #[test]
    fn tool_call_finalizes_assistant_and_records_summary() {
        let mut s = AppState::new("gpt-5", false);
        s.apply(&AgentEvent::Text("je lance".into()));
        s.apply(&AgentEvent::ToolCall(ToolCallView {
            id: "c1".into(),
            name: "bash".into(),
            input: serde_json::json!({ "command": "ls -la" }),
        }));
        assert!(matches!(
            s.blocks[0],
            Block::Assistant {
                streaming: false,
                ..
            }
        ));
        assert_eq!(
            s.blocks[1],
            Block::ToolCall {
                name: "bash".into(),
                summary: "ls -la".into()
            }
        );
    }

    #[test]
    fn tool_result_carries_taint_and_error() {
        let mut s = AppState::new("gpt-5", false);
        s.apply(&AgentEvent::ToolResult(ToolResultView {
            id: "c1".into(),
            content: "oops".into(),
            is_error: true,
            untrusted: true,
        }));
        assert_eq!(
            s.blocks[0],
            Block::ToolResult {
                content: "oops".into(),
                untrusted: true,
                is_error: true
            }
        );
    }

    #[test]
    fn typing_and_submit_produces_action_and_clears_input() {
        let mut s = AppState::new("gpt-5", false);
        for c in "salut".chars() {
            assert_eq!(s.on_key(key(c)), InputAction::None);
        }
        assert_eq!(s.input, "salut");
        let action = s.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, InputAction::Submit("salut".into()));
        assert!(s.input.is_empty());
    }

    #[test]
    fn empty_submit_is_noop() {
        let mut s = AppState::new("gpt-5", false);
        assert_eq!(
            s.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            InputAction::None
        );
    }

    #[test]
    fn permission_mode_routes_keys() {
        let mut s = AppState::new("gpt-5", false);
        s.pending = Some(PermissionPrompt {
            title: "bash".into(),
            reason: "sensible".into(),
            detail: vec![],
        });
        // une frappe normale ne tape PAS dans l'input pendant la confirmation
        assert_eq!(s.on_key(key('x')), InputAction::None);
        assert!(s.input.is_empty());
        // 'o' accepte
        assert_eq!(s.on_key(key('o')), InputAction::Permission(true));
        assert!(s.pending.is_none());
    }

    #[test]
    fn ctrl_c_quits() {
        let mut s = AppState::new("gpt-5", false);
        let action = s.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Quit);
        assert!(s.should_quit);
    }
}
