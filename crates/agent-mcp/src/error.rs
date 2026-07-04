//! Erreurs de la couche MCP : configuration, démarrage et connexion d'un serveur.

use std::path::PathBuf;

/// Erreur de chargement de config ou de connexion à un serveur MCP. Le message
/// (Display) inline la cause — suffisant pour l'affichage TUI.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("lecture de {0} : {1}")]
    Read(PathBuf, std::io::Error),
    #[error("JSON invalide dans {0} : {1}")]
    Parse(PathBuf, serde_json::Error),
    #[error("server \"{server}\": failed to start process: {source}")]
    Spawn {
        server: String,
        source: std::io::Error,
    },
    #[error("serveur « {server} » : {message}")]
    Connect { server: String, message: String },
    #[error("serveur MCP « {0} » inconnu")]
    Unknown(String),
}
