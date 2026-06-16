//! Parsing de `.mcp.json` (format compatible Claude Code). MVP : transport stdio
//! uniquement. Une entrée sans `command` (serveur SSE / HTTP) est ignorée, pas
//! fatale — ces transports arrivent en Phase 2.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::McpError;

/// Configuration d'un serveur MCP stdio : commande + arguments + variables d'env.
/// Les champs inconnus du JSON (`type`, `disabled`, …) sont ignorés.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawConfigFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, serde_json::Value>,
}

/// Contenu résolu de `.mcp.json` : les serveurs stdio exploitables (ordre stable).
#[derive(Debug, Clone, Default)]
pub struct McpConfigFile {
    pub servers: BTreeMap<String, McpServerConfig>,
    /// Entrées ignorées car non-stdio (sans `command`) — reportées Phase 2.
    pub skipped: usize,
}

impl McpConfigFile {
    /// Charge `<dir>/.mcp.json` (config MCP du workspace). Fichier absent → config
    /// vide (ce n'est pas une erreur : la plupart des workspaces n'en ont pas).
    pub fn load(dir: &Path) -> Result<Self, McpError> {
        Self::load_file(&dir.join(".mcp.json"))
    }

    /// Charge les `mcpServers` user-scope d'un fichier Claude Code (`~/.claude.json`)
    /// pour réutiliser les serveurs déjà installés. Même clé `mcpServers` et même
    /// forme stdio que `.mcp.json` ; tous les autres champs du fichier sont ignorés.
    pub fn load_claude(path: &Path) -> Result<Self, McpError> {
        Self::load_file(path)
    }

    /// Fusionne `lower` SOUS `self` : en cas de collision de nom, le serveur de
    /// `self` (priorité haute, ex. workspace) l'emporte sur celui de `lower`
    /// (ex. user-scope Claude Code).
    #[must_use]
    pub fn merge_under(mut self, lower: McpConfigFile) -> Self {
        for (name, cfg) in lower.servers {
            self.servers.entry(name).or_insert(cfg);
        }
        self.skipped += lower.skipped;
        self
    }

    /// Lit un fichier JSON portant une clé `mcpServers` et en extrait les serveurs
    /// stdio (les entrées sans `command` — remote SSE/HTTP — sont comptées dans
    /// `skipped` et ignorées). Fichier absent → config vide.
    fn load_file(path: &Path) -> Result<Self, McpError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(McpError::Read(path.to_path_buf(), e)),
        };
        let file: RawConfigFile =
            serde_json::from_str(&raw).map_err(|e| McpError::Parse(path.to_path_buf(), e))?;

        let mut servers = BTreeMap::new();
        let mut skipped = 0;
        for (name, value) in file.mcp_servers {
            match serde_json::from_value::<McpServerConfig>(value) {
                Ok(cfg) => {
                    servers.insert(name, cfg);
                }
                Err(_) => skipped += 1,
            }
        }
        Ok(Self { servers, skipped })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(command: &str) -> McpServerConfig {
        McpServerConfig {
            command: command.to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn merge_under_keeps_high_priority_on_collision() {
        let mut high = BTreeMap::new();
        high.insert("a".to_string(), server("high"));
        let high = McpConfigFile {
            servers: high,
            skipped: 0,
        };

        let mut low = BTreeMap::new();
        low.insert("a".to_string(), server("low"));
        low.insert("b".to_string(), server("low-b"));
        let low = McpConfigFile {
            servers: low,
            skipped: 2,
        };

        let merged = high.merge_under(low);
        // Collision « a » → la priorité haute (workspace) gagne.
        assert_eq!(merged.servers.get("a").unwrap().command, "high");
        // « b » est ajouté depuis la priorité basse.
        assert_eq!(merged.servers.get("b").unwrap().command, "low-b");
        assert_eq!(merged.skipped, 2);
    }
}
