//! `agent-session` — persistance JSONL append-only + resume par dossier (US-009,
//! ARCHITECTURE §7). Implémente le trait `Session` d'`agent-core` (injecté dans
//! la boucle). Dépend d'`agent-core` pour les types canoniques.
//!
//! Garanties :
//! - **durabilité par entrée** : chaque entrée est sérialisée puis `write_all` +
//!   `flush` + `sync_data` (fdatasync). Note : `write_all` peut émettre plusieurs
//!   syscalls `write()` ; un crash OS au milieu peut laisser une ligne PARTIELLE
//!   en queue de fichier — c'est précisément ce que le resume détecte et ignore
//!   (dernière ligne non parsable, AC3). On ne promet donc pas « tout ou rien »
//!   au niveau octet, mais « toute ligne incomplète en queue est ignorée ».
//! - **resume** : on rejoue le log ; une dernière ligne tronquée par un crash en
//!   plein écrit est ignorée (AC3), la session reprend au dernier état valide.
//! - une `CompactBoundary` réinitialise le transcript reconstruit (les messages
//!   d'avant ont été compactés). Le `clear` est **différé** jusqu'au premier
//!   `Message` suivant : une frontière orpheline (crash entre frontière et
//!   résumé) n'efface alors PAS le transcript antérieur.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use agent_core::CompactKind;
use agent_core::message::Message;
use agent_core::session::{FileSnapshot, Session, SessionEntry, SessionError};

/// Nom du fichier de session dans un dossier de travail.
pub const SESSION_FILE: &str = "session.jsonl";

fn io_err(e: impl std::fmt::Display) -> SessionError {
    SessionError::Io(e.to_string())
}
fn serde_err(e: impl std::fmt::Display) -> SessionError {
    SessionError::Serde(e.to_string())
}

/// Session JSONL append-only. Tient un curseur du nombre de messages déjà écrits
/// pour que `sync` n'écrive que le delta (transcript-before-response idempotent).
pub struct JsonlSession {
    file: Mutex<File>,
    cursor: Mutex<usize>,
}

impl JsonlSession {
    /// Crée (ou rouvre en append) le fichier de session dans `dir`.
    pub fn create_in(dir: &Path) -> Result<Self, SessionError> {
        std::fs::create_dir_all(dir).map_err(io_err)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(SESSION_FILE))
            .map_err(io_err)?;
        Ok(Self {
            file: Mutex::new(file),
            cursor: Mutex::new(0),
        })
    }

    fn append(&self, entry: &SessionEntry) -> Result<(), SessionError> {
        let line = format!("{}\n", serde_json::to_string(entry).map_err(serde_err)?);
        self.write_locked(&line)
    }

    /// Écrit un buffer déjà sérialisé (≥ 1 ligne) sous le verrou fichier, avec
    /// durabilité (`flush` + `sync_data`).
    fn write_locked(&self, buf: &str) -> Result<(), SessionError> {
        let mut f = self
            .file
            .lock()
            .map_err(|_| SessionError::Io("verrou fichier empoisonné".into()))?;
        f.write_all(buf.as_bytes()).map_err(io_err)?;
        f.flush().map_err(io_err)?;
        f.sync_data().map_err(io_err)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Session for JsonlSession {
    async fn sync(&self, messages: &[Message]) -> Result<(), SessionError> {
        let mut cur = self
            .cursor
            .lock()
            .map_err(|_| SessionError::Io("verrou curseur empoisonné".into()))?;
        let start = (*cur).min(messages.len());
        for m in &messages[start..] {
            self.append(&SessionEntry::Message(m.clone()))?;
        }
        *cur = messages.len();
        Ok(())
    }

    async fn checkpoint(
        &self,
        kind: CompactKind,
        messages: &[Message],
    ) -> Result<(), SessionError> {
        // Frontière + transcript post-compaction écrits en UN SEUL write_locked :
        // pas de fenêtre où une frontière existe sans son résumé (#9).
        let mut buf = format!(
            "{}\n",
            serde_json::to_string(&SessionEntry::CompactBoundary { kind }).map_err(serde_err)?
        );
        for m in messages {
            buf.push_str(
                &serde_json::to_string(&SessionEntry::Message(m.clone())).map_err(serde_err)?,
            );
            buf.push('\n');
        }
        self.write_locked(&buf)?;
        *self
            .cursor
            .lock()
            .map_err(|_| SessionError::Io("verrou curseur empoisonné".into()))? = messages.len();
        Ok(())
    }

    async fn record_file_snapshot(&self, snapshot: FileSnapshot) -> Result<(), SessionError> {
        self.append(&SessionEntry::FileHistorySnapshot(snapshot))
    }
}

/// État reconstruit depuis un log de session.
#[derive(Debug, Default)]
pub struct ResumedSession {
    pub messages: Vec<Message>,
    pub compactions: usize,
    /// Vrai si une dernière ligne partielle (crash mid-write) a été ignorée.
    pub skipped_partial: bool,
}

/// Reprend la session d'un dossier (`<dir>/session.jsonl`).
pub fn resume_dir(dir: &Path) -> Result<ResumedSession, SessionError> {
    resume_file(&dir.join(SESSION_FILE))
}

/// Reprend une session depuis un fichier JSONL. Fichier absent ⇒ session vide.
pub fn resume_file(path: &Path) -> Result<ResumedSession, SessionError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ResumedSession::default());
        }
        Err(e) => return Err(io_err(e)),
    };

    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    let mut out = ResumedSession::default();
    // clear DIFFÉRÉ : une frontière n'efface le transcript antérieur que lorsque
    // son premier Message de résumé arrive. Une frontière orpheline (crash entre
    // frontière et résumé) préserve donc le transcript d'avant.
    let mut pending_clear = false;

    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionEntry>(line) {
            Ok(SessionEntry::Message(m)) => {
                if pending_clear {
                    out.messages.clear();
                    pending_clear = false;
                }
                out.messages.push(m);
            }
            Ok(SessionEntry::CompactBoundary { .. }) => {
                pending_clear = true;
                out.compactions += 1;
            }
            Ok(SessionEntry::FileHistorySnapshot(_)) => {}
            Err(e) => {
                if i == n - 1 {
                    // dernière ligne tronquée par un crash → ignorée (AC3).
                    out.skipped_partial = true;
                } else {
                    return Err(SessionError::Serde(format!("ligne {i} corrompue: {e}")));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::Message;

    /// Dossier temporaire isolé par test (pas de dépendance `rand`/`tempfile`).
    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("numen_sess_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn write_then_resume_roundtrip() {
        let dir = tmp("roundtrip");
        let s = JsonlSession::create_in(&dir).unwrap();
        let msgs = vec![Message::user("salut"), Message::assistant_text("bonjour")];
        s.sync(&msgs).await.unwrap();
        // re-sync idempotent : n'ajoute rien
        s.sync(&msgs).await.unwrap();

        let resumed = resume_dir(&dir).unwrap();
        assert_eq!(resumed.messages.len(), 2);
        assert_eq!(resumed.messages[0].text(), "salut");
        assert_eq!(resumed.messages[1].text(), "bonjour");
        assert!(!resumed.skipped_partial);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn checkpoint_resets_transcript() {
        let dir = tmp("compact");
        let s = JsonlSession::create_in(&dir).unwrap();
        s.sync(&[Message::user("vieux 1"), Message::assistant_text("vieux 2")])
            .await
            .unwrap();
        // checkpoint atomique : frontière + transcript post-compaction ([résumé]).
        s.checkpoint(CompactKind::Auto, &[Message::user("[résumé]")])
            .await
            .unwrap();

        let resumed = resume_dir(&dir).unwrap();
        assert_eq!(resumed.compactions, 1);
        assert_eq!(
            resumed.messages.len(),
            1,
            "les vieux messages sont compactés"
        );
        assert_eq!(resumed.messages[0].text(), "[résumé]");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // #9 : une frontière ORPHELINE (crash entre frontière et résumé) ne doit PAS
    // effacer le transcript antérieur (clear différé).
    #[test]
    fn dangling_boundary_preserves_prior_transcript() {
        let dir = tmp("dangling");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SESSION_FILE);
        let msg = serde_json::to_string(&SessionEntry::Message(Message::user("avant"))).unwrap();
        let boundary = serde_json::to_string(&SessionEntry::CompactBoundary {
            kind: CompactKind::Auto,
        })
        .unwrap();
        // ...message, frontière, PUIS rien (crash avant l'écriture du résumé)
        std::fs::write(&path, format!("{msg}\n{boundary}\n")).unwrap();

        let resumed = resume_file(&path).unwrap();
        assert_eq!(resumed.compactions, 1);
        assert_eq!(resumed.messages.len(), 1, "le transcript antérieur survit");
        assert_eq!(resumed.messages[0].text(), "avant");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // US-009 AC1 : l'entrée discriminée FileHistorySnapshot s'écrit et est ignorée
    // proprement au resume.
    #[tokio::test]
    async fn file_snapshot_roundtrips_and_is_skipped() {
        let dir = tmp("snapshot");
        let s = JsonlSession::create_in(&dir).unwrap();
        s.sync(&[Message::user("hi")]).await.unwrap();
        s.record_file_snapshot(FileSnapshot {
            path: "src/main.rs".into(),
            content: "fn main() {}".into(),
        })
        .await
        .unwrap();

        let resumed = resume_dir(&dir).unwrap();
        // le snapshot est une entrée valide, ignorée pour la reconstruction du transcript
        assert_eq!(resumed.messages.len(), 1);
        assert!(!resumed.skipped_partial);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_skips_truncated_last_line() {
        let dir = tmp("truncated");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SESSION_FILE);
        // une entrée valide + une ligne partielle (crash mid-write, pas de \n final)
        let valid = serde_json::to_string(&SessionEntry::Message(Message::user("ok"))).unwrap();
        std::fs::write(
            &path,
            format!("{valid}\n{{\"entry\":\"message\",\"role\":\"us"),
        )
        .unwrap();

        let resumed = resume_file(&path).unwrap();
        assert!(
            resumed.skipped_partial,
            "la ligne tronquée doit être ignorée"
        );
        assert_eq!(resumed.messages.len(), 1);
        assert_eq!(resumed.messages[0].text(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_missing_file_is_empty() {
        let dir = tmp("missing");
        let resumed = resume_dir(&dir).unwrap();
        assert!(resumed.messages.is_empty());
        assert_eq!(resumed.compactions, 0);
    }

    #[test]
    fn resume_corrupt_middle_line_errors() {
        let dir = tmp("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SESSION_FILE);
        let valid = serde_json::to_string(&SessionEntry::Message(Message::user("ok"))).unwrap();
        // ligne corrompue AU MILIEU (suivie d'une ligne valide) → vraie corruption
        std::fs::write(&path, format!("{valid}\nGARBAGE\n{valid}\n")).unwrap();
        assert!(resume_file(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
