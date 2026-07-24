//! Gestion de la credential OAuth de l'abonnement ChatGPT pour l'adapter :
//! refresh **rotatif** sous verrou, persistance keyring, et fabrication des
//! en-têtes d'inférence (délègue à `agent-auth`).
//!
//! `Provider::stream` prend `&self` → la credential vit derrière un
//! `tokio::sync::Mutex` (interior mutability ; refresh réseau possible sous lock).

use agent_auth::oauth::openai_chatgpt::{self, AuthError, RequestSpec};
use agent_auth::{Credential, OAuthCredential};
use agent_core::provider::ProviderError;

/// Marge de refresh : on rafraîchit 60 s AVANT l'expiration pour éviter une course
/// expiry/requête (Pi vise le bord exact ; la marge est plus robuste).
const REFRESH_MARGIN_MS: u64 = 60_000;

pub struct CredentialManager {
    state: tokio::sync::Mutex<CredentialState>,
    http: reqwest::Client,
    /// Clé keyring où réécrire la credential rafraîchie (refresh rotatif).
    keyring_account: String,
}

struct CredentialState {
    cred: Option<OAuthCredential>,
    persist_dirty: bool,
}

impl CredentialManager {
    pub fn new(
        cred: OAuthCredential,
        http: reqwest::Client,
        keyring_account: impl Into<String>,
    ) -> Self {
        Self {
            state: tokio::sync::Mutex::new(CredentialState {
                cred: Some(cred),
                persist_dirty: false,
            }),
            http,
            keyring_account: keyring_account.into(),
        }
    }

    /// Garantit un access token frais (refresh + réécriture keyring si nécessaire)
    /// et retourne la spec de requête d'inférence (URL + en-têtes propriétaires).
    pub async fn request_spec(&self) -> Result<RequestSpec, ProviderError> {
        self.fresh_spec(openai_chatgpt::responses_request).await
    }

    /// Idem `request_spec` pour la découverte du catalogue de modèles (`/models`).
    pub async fn models_spec(&self) -> Result<RequestSpec, ProviderError> {
        self.fresh_spec(openai_chatgpt::models_request).await
    }

    /// Garantit un access token frais puis fabrique la spec via `build`.
    async fn fresh_spec(
        &self,
        build: fn(&OAuthCredential) -> Result<RequestSpec, AuthError>,
    ) -> Result<RequestSpec, ProviderError> {
        let mut state = self.state.lock().await;
        let now = openai_chatgpt::now_ms();
        if state.cred.is_none() {
            return Err(disconnected_error());
        }
        if state.persist_dirty {
            let cred = state.cred.as_ref().ok_or_else(disconnected_error)?;
            self.persist(cred).await?;
            state.persist_dirty = false;
        }
        let cred = state.cred.as_mut().ok_or_else(disconnected_error)?;
        if now.saturating_add(REFRESH_MARGIN_MS) >= cred.expires_at {
            self.refresh_locked(&mut state, now).await?;
        }
        let cred = state.cred.as_ref().ok_or_else(disconnected_error)?;
        build(cred).map_err(convert_auth_err)
    }

    /// Force un refresh même si l'horloge locale pense encore le token valide.
    pub async fn force_refresh(&self) -> Result<(), ProviderError> {
        let mut state = self.state.lock().await;
        if state.cred.is_none() {
            return Err(disconnected_error());
        }
        self.refresh_locked(&mut state, openai_chatgpt::now_ms())
            .await
    }

    /// Invalide la credential en mémoire. Utilisé par le logout interactif après
    /// suppression keyring pour empêcher une résurrection au prochain refresh.
    pub async fn disconnect(&self) {
        let mut state = self.state.lock().await;
        state.cred = None;
        state.persist_dirty = false;
    }

    async fn refresh_locked(
        &self,
        state: &mut CredentialState,
        now: u64,
    ) -> Result<(), ProviderError> {
        let refresh_token = state
            .cred
            .as_ref()
            .ok_or_else(disconnected_error)?
            .refresh
            .expose()
            .to_string();
        let refreshed = openai_chatgpt::refresh(&self.http, &refresh_token, now)
            .await
            .map_err(convert_auth_err)?;
        state.cred = Some(refreshed.clone());
        state.persist_dirty = true;
        self.persist(&refreshed).await?;
        state.persist_dirty = false;
        Ok(())
    }

    /// Réécrit la credential rafraîchie dans le keyring (op bloquante → hors
    /// runtime async).
    async fn persist(&self, cred: &OAuthCredential) -> Result<(), ProviderError> {
        let account = self.keyring_account.clone();
        let blob = Credential::Oauth(cred.clone());
        tokio::task::spawn_blocking(move || agent_auth::store::save(&account, &blob))
            .await
            .map_err(|e| ProviderError::Transport(format!("join keyring: {e}")))?
            .map_err(|e| ProviderError::Transport(format!("keyring: {e}")))
    }
}

fn disconnected_error() -> ProviderError {
    ProviderError::Http {
        status: 401,
        message: "auth disconnected".to_string(),
        retry_after_ms: None,
    }
}

/// Mappe une erreur d'auth vers `ProviderError` en préservant la sémantique de
/// retry : un refresh rejeté en 401/403 (refresh révoqué / client Codex coupé) est
/// **fatal** (`Http` → `Auth` côté `classify_error`), pas un retry transitoire.
fn convert_auth_err(e: AuthError) -> ProviderError {
    match e {
        AuthError::Http(re) => match re.status() {
            Some(s) if s.as_u16() == 401 || s.as_u16() == 403 => ProviderError::Http {
                status: s.as_u16(),
                message: "OAuth refresh rejected (revoked token?)".to_string(),
                retry_after_ms: None,
            },
            Some(s) => ProviderError::Http {
                status: s.as_u16(),
                message: re.to_string(),
                retry_after_ms: None,
            },
            None => ProviderError::Transport(re.to_string()),
        },
        other => ProviderError::Transport(other.to_string()),
    }
}
