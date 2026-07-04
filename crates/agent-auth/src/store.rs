//! Stockage des credentials dans le secret store de l'OS (US-018) — Secret
//! Service / keyring, jamais en clair sur disque. On NE réplique PAS le
//! `~/.pi/agent/auth.json` clair de Pi : le blob JSON (tokens inclus) vit dans
//! le keyring, chiffré par l'OS.
//!
//! On utilise `set_secret` plutôt que `set_password` : sur Windows, `keyring`
//! encode les passwords en UTF-16 avant `CredWriteW`, ce qui divise inutilement
//! la taille disponible pour les tokens OAuth.

use crate::Credential;

const SERVICE: &str = "pyxis";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("secret store unavailable: {0} (fallback: env var, see docs)")]
    Keyring(#[from] keyring::Error),
    #[error("credential serialization/deserialization: {0}")]
    Serde(#[from] serde_json::Error),
}

fn entry(account: &str) -> Result<keyring::Entry, StoreError> {
    Ok(keyring::Entry::new(SERVICE, account)?)
}

/// Persiste une credential (blob JSON) dans le keyring sous la clé `account`
/// (typiquement `oauth:openai_chatgpt` ou `apikey:openai_chat`).
pub fn save(account: &str, cred: &Credential) -> Result<(), StoreError> {
    let blob = serde_json::to_vec(cred)?;
    entry(account)?.set_secret(&blob)?;
    Ok(())
}

/// Lit une credential, `None` si absente.
pub fn load(account: &str) -> Result<Option<Credential>, StoreError> {
    let entry = entry(account)?;
    match entry.get_secret() {
        Ok(blob) => match serde_json::from_slice(&blob) {
            Ok(cred) => Ok(Some(cred)),
            Err(secret_err) => match entry.get_password() {
                Ok(password_blob) => Ok(Some(serde_json::from_str(&password_blob)?)),
                Err(keyring::Error::NoEntry) => Err(secret_err.into()),
                Err(e) => Err(e.into()),
            },
        },
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Supprime une credential (idempotent : absente == succès).
pub fn delete(account: &str) -> Result<(), StoreError> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
