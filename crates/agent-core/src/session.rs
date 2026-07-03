//! Contrat de persistance de session (injecté). L'implémentation JSONL
//! append-only + resume est `agent-session` (US-009) ; le cœur ne connaît que
//! ce trait et les types d'entrée canoniques.

use serde::{Deserialize, Serialize};

use crate::compaction::CompactKind;
use crate::message::Message;

/// Entrée de log discriminée (ARCHITECTURE §7). Sérialisée une par ligne JSONL.
/// `CompactBoundary` reste lisible pour les anciens logs ; les nouveaux
/// checkpoints de compaction passent par `CompactCheckpoint`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum SessionEntry {
    Meta {
        schema_version: u32,
    },
    Message(Message),
    CompactBoundary {
        kind: CompactKind,
    },
    CompactCheckpoint {
        kind: CompactKind,
        messages: Vec<Message>,
    },
    EncryptedReasoningRedacted,
    FileHistorySnapshot(FileSnapshot),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(String),
    #[error("serde: {0}")]
    Serde(String),
}

#[async_trait::async_trait]
pub trait Session: Send + Sync {
    /// Persiste les messages pas encore écrits (transcript-before-response,
    /// invariant 6). DOIT être idempotent : n'écrit que le delta depuis le
    /// dernier `sync` (l'implémentation tient un curseur).
    async fn sync(&self, messages: &[Message]) -> Result<(), SessionError>;

    /// Checkpoint de compaction **full** (auto/reactive) : écrit le transcript
    /// post-compaction comme une entrée replayable unique, puis resynchronise le
    /// curseur sur `messages.len()`. La microcompaction, elle, est purement en
    /// mémoire et n'appelle PAS ceci.
    async fn checkpoint(&self, kind: CompactKind, messages: &[Message])
    -> Result<(), SessionError>;

    /// Enregistre une redaction durable des blocs de reasoning chiffrés déjà
    /// persistés. Le replay applique cette redaction aux messages reconstruits.
    async fn redact_encrypted_reasoning(&self) -> Result<(), SessionError>;

    /// Écrit un snapshot de fichier (entrée discriminée `FileHistorySnapshot`).
    async fn record_file_snapshot(&self, snapshot: FileSnapshot) -> Result<(), SessionError>;
}
