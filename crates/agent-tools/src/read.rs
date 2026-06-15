//! Outil `read` — lit un fichier du workspace avec numéros de ligne. Read-only,
//! concurrency-safe, sortie untrusted (le contenu lu peut porter une injection,
//! OWASP LLM01). US-011 AC1/AC3.

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::ToolError;
use crate::path::confine;
use crate::permission::{PermCtx, PermissionDecision};
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Au-delà, on considère le contenu binaire/illisible (présence d'octets NUL
/// vérifiée séparément ; ceci borne juste la taille lue en MVP).
const MAX_BYTES: usize = 2_000_000;

#[derive(Debug, Deserialize)]
pub struct ReadInput {
    pub path: String,
    /// Ligne de départ (1-indexée). Défaut : 1.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Nombre de lignes max. Défaut : tout.
    #[serde(default)]
    pub limit: Option<usize>,
}

pub struct Read;

#[async_trait]
impl Tool for Read {
    type Input = ReadInput;

    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> String {
        "Lit un fichier texte du workspace et retourne son contenu préfixé des \
         numéros de ligne. Paramètres : path (relatif au workspace), offset \
         (ligne de départ 1-indexée, optionnel), limit (nombre de lignes, \
         optionnel)."
            .to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Chemin du fichier (relatif au workspace)." },
                "offset": { "type": "integer", "minimum": 1, "description": "Ligne de départ (1-indexée)." },
                "limit": { "type": "integer", "minimum": 1, "description": "Nombre de lignes maximum." }
            },
            "required": ["path"]
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn is_concurrency_safe(&self) -> bool {
        true
    }
    fn is_sensitive(&self) -> bool {
        false
    }
    fn permission(&self, _input: &Self::Input, _ctx: &PermCtx) -> PermissionDecision {
        PermissionDecision::Allow
    }

    async fn call(&self, input: Self::Input, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let path = confine(&ctx.workspace, &input.path)?;
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::Io(format!("{}: {e}", input.path)))?;
        if meta.is_dir() {
            return Err(ToolError::Rejected(format!(
                "{} est un répertoire, pas un fichier",
                input.path
            )));
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::Io(format!("{}: {e}", input.path)))?;
        if bytes.contains(&0) {
            return Err(ToolError::Rejected(format!(
                "{} semble être un fichier binaire (octets NUL)",
                input.path
            )));
        }
        if bytes.len() > MAX_BYTES {
            return Err(ToolError::Rejected(format!(
                "{} dépasse {} octets (lecture partielle non supportée en MVP)",
                input.path, MAX_BYTES
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let start = input.offset.unwrap_or(1).max(1);
        let mut out = String::new();
        let mut count = 0usize;
        for (idx, line) in text.lines().enumerate() {
            let lineno = idx + 1;
            if lineno < start {
                continue;
            }
            if input.limit.is_some_and(|limit| count >= limit) {
                break;
            }
            out.push_str(&format!("{lineno:>6}\t{line}\n"));
            count += 1;
        }
        if out.is_empty() {
            out.push_str("(fichier vide ou plage hors limites)");
        }
        Ok(ToolOutput::text(out))
    }
}
