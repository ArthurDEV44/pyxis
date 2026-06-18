//! `TuiApprover` — pont entre le pipeline d'outils (`agent_tools::Approver`) et le
//! frontend : envoie la demande de permission à la boucle TUI et attend la
//! réponse (oneshot). Traduit la `PermissionRequest` en `PermissionPrompt`
//! (avec aperçu diff pour `edit`) consommé par le rendu.
//!
//! Fail-closed : si le canal est fermé (TUI partie) ou la réponse perdue, on
//! **refuse** par défaut.

use agent_tools::permission::{Approver, PermissionRequest};
use agent_tui::{PermissionPrompt, diff};
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

/// Construit le prompt visuel depuis la demande : titre adapté à l'outil + aperçu
/// via le MÊME moteur de diff que le transcript (`diff::from_tool` pour `edit` /
/// `write` ; lignes de contexte pour `bash` / inconnu). US-039.
pub fn to_prompt(req: &PermissionRequest) -> PermissionPrompt {
    let v = &req.input;
    let str_field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();

    let (title, preview) = match req.tool.as_str() {
        "edit" => (
            format!("edit {}", str_field("path")),
            diff::from_tool("edit", v).unwrap_or_default(),
        ),
        "write" => (
            format!("write {}", str_field("path")),
            diff::from_tool("write", v).unwrap_or_default(),
        ),
        "bash" => (
            "bash".to_string(),
            diff::note([str_field("command").to_string()]),
        ),
        // `note` attend une ligne par item : on splitte (un résumé multi-lignes ne
        // doit pas se retrouver en un seul `Row::Context` avec des `\n` embarqués).
        other => (
            other.to_string(),
            diff::note(req.input_summary.lines().map(str::to_string)),
        ),
    };

    PermissionPrompt {
        title,
        reason: req.reason.clone(),
        preview,
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
        use agent_tui::diff::Row;
        let p = to_prompt(&req(
            "edit",
            serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
        ));
        assert_eq!(p.title, "edit a.rs");
        assert!(
            p.preview
                .rows
                .iter()
                .any(|r| matches!(r, Row::Remove { .. }))
        );
        assert!(p.preview.rows.iter().any(|r| matches!(r, Row::Add { .. })));
    }

    #[test]
    fn bash_request_shows_command() {
        use agent_tui::diff::Row;
        let p = to_prompt(&req(
            "bash",
            serde_json::json!({ "command": "rm -rf /tmp/x" }),
        ));
        assert_eq!(p.title, "bash");
        assert!(matches!(&p.preview.rows[0], Row::Context { text, .. } if text == "rm -rf /tmp/x"));
    }
}
