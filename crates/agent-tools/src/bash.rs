//! Outil `bash` — exécute une commande shell dans le workspace. Action SENSIBLE
//! (destructive/réseau possible) → cible de la défense taint (§4.6) et `Ask` par
//! défaut. Sortie untrusted (stdout/stderr = contenu externe). Le Registry
//! enveloppe l'appel dans un `timeout` ; `kill_on_drop` tue le process si le
//! timeout expire (US-012 AC2 / unhappy path US-003). US-012.

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{ToolError, ValidationError};
use crate::permission::{PermCtx, PermissionDecision};
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Borne de capture (évite un flood de prompt sur une sortie géante).
const MAX_OUTPUT: usize = 30_000;

#[derive(Debug, Deserialize)]
pub struct BashInput {
    pub command: String,
}

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    type Input = BashInput;

    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> String {
        "Exécute une commande shell (sh -c) dans le workspace et retourne \
         stdout/stderr et le code de sortie. La commande tourne sous timeout. \
         Paramètre : command."
            .to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Commande shell à exécuter." }
            },
            "required": ["command"]
        })
    }
    // Defaults fail-closed conservés : non read-only, non concurrent, SENSIBLE,
    // untrusted. On les rend explicites pour la lisibilité.
    fn is_read_only(&self) -> bool {
        false
    }
    fn is_concurrency_safe(&self) -> bool {
        false
    }
    fn is_sensitive(&self) -> bool {
        true
    }
    fn returns_untrusted(&self) -> bool {
        true
    }
    fn validate_input(&self, input: &Self::Input) -> Result<(), ValidationError> {
        if input.command.trim().is_empty() {
            return Err(ValidationError::new("commande vide"));
        }
        Ok(())
    }
    fn permission(&self, _input: &Self::Input, _ctx: &PermCtx) -> PermissionDecision {
        PermissionDecision::Ask
    }

    async fn call(&self, input: Self::Input, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&input.command)
            .current_dir(&ctx.workspace)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());

        // Durcissement sandbox (réseau via HTTP_PROXY) injecté par l'agent-cli.
        // Le confinement FS Landlock est process-wide → hérité par ce sous-process.
        if let Some(harden) = &ctx.harden {
            harden(&mut cmd);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| ToolError::Io(format!("lancement du shell: {e}")))?;

        let mut body = String::new();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.is_empty() {
            body.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&stderr);
        }
        if body.len() > MAX_OUTPUT {
            body.truncate(MAX_OUTPUT);
            body.push_str("\n… (sortie tronquée)");
        }

        let code = output.status.code();
        match code {
            Some(0) => {
                if body.is_empty() {
                    body.push_str("(aucune sortie, succès)");
                }
                Ok(ToolOutput::text(body))
            }
            Some(n) => {
                body.push_str(&format!("\n[code de sortie {n}]"));
                Ok(ToolOutput::error(body))
            }
            None => {
                body.push_str("\n[terminé par signal]");
                Ok(ToolOutput::error(body))
            }
        }
    }
}
