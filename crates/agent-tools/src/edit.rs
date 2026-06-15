//! Outil `edit` — remplacement ancré : `old_string` doit apparaître EXACTEMENT
//! une fois. 0 → ancre introuvable ; ≥ 2 → ancre ambiguë (échec explicite, AUCUNE
//! mutation, edge case #11 / US-012 AC1). Mutation confinée au workspace.

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{ToolError, ValidationError};
use crate::path::confine;
use crate::permission::{PermCtx, PermissionDecision};
use crate::tool::{Tool, ToolCtx, ToolOutput};

#[derive(Debug, Deserialize)]
pub struct EditInput {
    pub path: String,
    /// Texte à remplacer — doit être unique dans le fichier.
    pub old_string: String,
    pub new_string: String,
}

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    type Input = EditInput;

    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> String {
        "Remplace une occurrence unique de texte dans un fichier. old_string doit \
         être présent exactement une fois (sinon l'édition échoue sans rien \
         modifier). Paramètres : path, old_string, new_string."
            .to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Chemin du fichier (relatif au workspace)." },
                "old_string": { "type": "string", "description": "Texte à remplacer (ancre unique)." },
                "new_string": { "type": "string", "description": "Texte de remplacement." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    fn is_sensitive(&self) -> bool {
        false
    }
    fn returns_untrusted(&self) -> bool {
        false
    }
    fn validate_input(&self, input: &Self::Input) -> Result<(), ValidationError> {
        if input.old_string.is_empty() {
            return Err(ValidationError::new(
                "old_string vide : impossible d'ancrer l'édition",
            ));
        }
        if input.old_string == input.new_string {
            return Err(ValidationError::new(
                "old_string == new_string : édition sans effet",
            ));
        }
        Ok(())
    }
    fn permission(&self, _input: &Self::Input, _ctx: &PermCtx) -> PermissionDecision {
        PermissionDecision::Ask
    }

    async fn call(&self, input: Self::Input, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let path = confine(&ctx.workspace, &input.path)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::Io(format!("{}: {e}", input.path)))?;

        let count = content.matches(&input.old_string).count();
        match count {
            0 => Err(ToolError::Rejected(format!(
                "ancre introuvable dans {} : old_string n'apparaît pas",
                input.path
            ))),
            1 => {
                let updated = content.replacen(&input.old_string, &input.new_string, 1);
                tokio::fs::write(&path, updated.as_bytes())
                    .await
                    .map_err(|e| ToolError::Io(format!("{}: {e}", input.path)))?;
                Ok(ToolOutput::text(format!("Édité : {}", input.path)))
            }
            n => Err(ToolError::Rejected(format!(
                "ancre ambiguë ({n} correspondances) dans {} — précisez old_string",
                input.path
            ))),
        }
    }
}
