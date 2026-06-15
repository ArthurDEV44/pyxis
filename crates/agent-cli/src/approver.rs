//! `TuiApprover` — pont entre le pipeline d'outils (`agent_tools::Approver`) et le
//! frontend : envoie la demande de permission à la boucle TUI et attend la
//! réponse (oneshot). Traduit la `PermissionRequest` en `PermissionPrompt`
//! (avec aperçu diff pour `edit`) consommé par le rendu.
//!
//! Fail-closed : si le canal est fermé (TUI partie) ou la réponse perdue, on
//! **refuse** par défaut.

use agent_tools::permission::{Approver, PermissionRequest};
use agent_tui::{DiffKind, DiffLine, PermissionPrompt};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

/// Message envoyé à la boucle TUI : la demande + le canal de réponse.
pub type PermissionMsg = (PermissionRequest, oneshot::Sender<bool>);

pub struct TuiApprover {
    tx: mpsc::Sender<PermissionMsg>,
}

impl TuiApprover {
    pub fn new(tx: mpsc::Sender<PermissionMsg>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Approver for TuiApprover {
    async fn approve(&self, req: &PermissionRequest) -> bool {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self.tx.send((req.clone(), resp_tx)).await.is_err() {
            return false; // TUI fermée → fail-closed
        }
        resp_rx.await.unwrap_or(false) // réponse perdue → fail-closed
    }
}

/// Construit le prompt visuel depuis la demande : un aperçu adapté à l'outil
/// (diff `-`/`+` pour `edit`, commande pour `bash`, début de contenu pour
/// `write`).
pub fn to_prompt(req: &PermissionRequest) -> PermissionPrompt {
    let v = &req.input;
    let str_field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();

    let (title, detail) = match req.tool.as_str() {
        "edit" => {
            let path = str_field("path");
            let mut detail = Vec::new();
            for line in str_field("old_string").split('\n') {
                detail.push(DiffLine {
                    kind: DiffKind::Remove,
                    text: line.to_string(),
                });
            }
            for line in str_field("new_string").split('\n') {
                detail.push(DiffLine {
                    kind: DiffKind::Add,
                    text: line.to_string(),
                });
            }
            (format!("edit {path}"), detail)
        }
        "write" => {
            let path = str_field("path");
            let detail = str_field("content")
                .split('\n')
                .take(10)
                .map(|l| DiffLine {
                    kind: DiffKind::Add,
                    text: l.to_string(),
                })
                .collect();
            (format!("write {path}"), detail)
        }
        "bash" => {
            let detail = vec![DiffLine {
                kind: DiffKind::Context,
                text: str_field("command").to_string(),
            }];
            ("bash".to_string(), detail)
        }
        other => {
            let detail = vec![DiffLine {
                kind: DiffKind::Context,
                text: req.input_summary.clone(),
            }];
            (other.to_string(), detail)
        }
    };

    PermissionPrompt {
        title,
        reason: req.reason.clone(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(tool: &str, input: serde_json::Value) -> PermissionRequest {
        PermissionRequest {
            tool: tool.into(),
            reason: "test".into(),
            input_summary: input.to_string(),
            input,
        }
    }

    #[test]
    fn edit_request_becomes_diff() {
        let p = to_prompt(&req(
            "edit",
            serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
        ));
        assert_eq!(p.title, "edit a.rs");
        assert!(p.detail.contains(&DiffLine {
            kind: DiffKind::Remove,
            text: "x".into()
        }));
        assert!(p.detail.contains(&DiffLine {
            kind: DiffKind::Add,
            text: "y".into()
        }));
    }

    #[test]
    fn bash_request_shows_command() {
        let p = to_prompt(&req(
            "bash",
            serde_json::json!({ "command": "rm -rf /tmp/x" }),
        ));
        assert_eq!(p.title, "bash");
        assert_eq!(p.detail[0].text, "rm -rf /tmp/x");
    }
}
