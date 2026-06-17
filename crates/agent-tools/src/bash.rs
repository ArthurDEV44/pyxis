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
            body = truncate_tail(&body, MAX_OUTPUT);
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

/// Tronque `body` en gardant la QUEUE (tail) sur `max` octets (US-026) : sur une
/// sortie longue (compilation : warnings en tête, erreurs + exit code en queue),
/// le tail préserve l'information critique. Le point de coupe est aligné sur une
/// frontière de caractère UTF-8 (jamais de panic d'indexation).
fn truncate_tail(body: &str, max: usize) -> String {
    if body.len() <= max {
        return body.to_string();
    }
    let mut cut = body.len() - max;
    while cut < body.len() && !body.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[... sortie tronquée, {cut} octets, début omis]\n{}", &body[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_truncation_keeps_the_end_and_marks_omission() {
        // 10 lignes ; on tronque pour ne garder que la fin (où vivent erreurs/exit).
        let body: String = (0..10).map(|i| format!("ligne{i}\n")).collect();
        let out = truncate_tail(&body, 20);
        assert!(out.starts_with("[... sortie tronquée, "));
        assert!(out.contains("octets, début omis]"));
        assert!(out.contains("ligne9"), "la fin doit être conservée: {out}");
        assert!(!out.contains("ligne0"), "le début doit être omis: {out}");
    }

    #[test]
    fn tail_truncation_is_char_boundary_safe() {
        // coupe au milieu d'un flux multi-octets → pas de panic, frontière respectée.
        let body = "é".repeat(100); // 200 octets
        let out = truncate_tail(&body, 51);
        assert!(out.contains("début omis]"));
        // le suffixe conservé est de l'UTF-8 valide (aucune coupe mid-codepoint).
        assert!(out.ends_with('é'));
    }

    #[test]
    fn short_output_is_untouched() {
        assert_eq!(truncate_tail("court", 30_000), "court");
    }
}
