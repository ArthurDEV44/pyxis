//! Auth abonnement ChatGPT (ADR-10). Réutilise le client OAuth du **Codex CLI
//! officiel OSS** — flow PKCE S256 sur `auth.openai.com` (browser + device-code),
//! décodage JWT pour `chatgpt_account_id`, refresh tokens rotatifs.
//!
//! Constantes vérifiées verbatim contre le repo Pi (`packages/ai/src/utils/oauth/
//! openai-codex.ts` + `providers/openai-codex-responses.ts`, 45/45 confirmées).
//! Détail & sources : `docs/openai-subscription-auth.md`.
//!
//! ⚠️ Zone grise ToS : se fait passer pour Codex (client_id partagé), **révocable
//! unilatéralement par OpenAI** (cf. ADR-7 R1, ADR-10). Credential « fragile »,
//! jamais en chemin critique : c'est une commodité de dogfood derrière BYOK.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;

use super::pkce::Pkce;
use crate::{OAuthCredential, ProviderId, Secret};

// ───────────────── Constantes auth (auth.openai.com) — verbatim Pi ─────────────────

/// `client_id` du Codex CLI OSS (`openai-codex.ts:31`).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// URI affichée à l'utilisateur en device flow.
pub const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// `redirect_uri` de l'échange code→token en **device** flow (≠ browser).
pub const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub const SCOPE: &str = "openid profile email offline_access";
pub const CALLBACK_PORT: u16 = 1455;
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(900);
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEVICE_CODE_TIMEOUT: Duration = Duration::from_secs(900);
/// Namespace du claim custom où vit `chatgpt_account_id` (`openai-codex.ts:44`).
pub const JWT_CLAIM_NAMESPACE: &str = "https://api.openai.com/auth";
/// ⚠️ Hardcodé par client (Pi met `"pi"`). Le backend ChatGPT **peut** valider
/// l'`originator` contre une liste connue — à tester au premier run (ADR-10).
pub const ORIGINATOR: &str = "pyxis";

/// Fallback `originator` si le backend rejette `pyxis` (US-021, unhappy path) :
/// emprunter l'identité du Codex CLI officiel OSS, déjà sur la liste blanche du
/// backend. Bascule à chaud via `PYXIS_ORIGINATOR` (pas de recompilation).
pub const ORIGINATOR_FALLBACK: &str = "codex_cli_rs";

/// `originator` effectif envoyé sur la requête d'INFÉRENCE (US-021). Lit
/// `PYXIS_ORIGINATOR` (permet de basculer `pyxis` ↔ `codex_cli_rs` pendant le
/// spike sans recompiler) ; défaut `ORIGINATOR`. N'affecte PAS le flow OAuth :
/// `build_authorize_url` garde `ORIGINATOR` (changer l'auth casserait le flow
/// validé en live, hors scope).
pub fn originator() -> String {
    match std::env::var("PYXIS_ORIGINATOR") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => ORIGINATOR.to_string(),
    }
}

/// Sélection déterministe du fallback (US-021, AC2) : `pyxis` si le backend
/// l'accepte, sinon `codex_cli_rs` (whitelisté). Pur/testable, indépendant de l'env.
pub fn originator_for(pyxis_accepted: bool) -> &'static str {
    if pyxis_accepted {
        ORIGINATOR
    } else {
        ORIGINATOR_FALLBACK
    }
}

// ───────────────── Constantes inférence (backend ChatGPT, Responses API) ─────────────────

pub const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const RESPONSES_PATH: &str = "/responses";
pub const OPENAI_BETA_SSE: &str = "responses=experimental";

// ──────────────────────────────── Erreurs ────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("erreur HTTP : {0}")]
    Http(#[from] reqwest::Error),
    #[error("réponse token invalide : {0}")]
    TokenResponse(String),
    #[error("JWT illisible : {0}")]
    Jwt(String),
    #[error("chatgpt_account_id absent du token")]
    MissingAccountId,
    #[error("credential provider inattendu : {0:?}")]
    WrongProvider(ProviderId),
    #[error("callback OAuth : {0}")]
    Callback(String),
    #[error("state OAuth ne correspond pas (anti-CSRF)")]
    StateMismatch,
    #[error("device flow expiré (900 s)")]
    DeviceTimeout,
    #[error("device flow refusé : {0}")]
    DeviceDenied(String),
    #[error("io : {0}")]
    Io(#[from] std::io::Error),
}

// ──────────────────────────── Wire types ────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

// ──────────────────────────── Builders purs (testables) ────────────────────────────

/// Construit l'URL d'autorisation (browser flow). Inclut les paramètres
/// non-standard exigés par le backend Codex (`id_token_add_organizations`,
/// `codex_cli_simplified_flow`).
pub fn build_authorize_url(challenge: &str, state: &str) -> Result<String, AuthError> {
    let mut url = url::Url::parse(AUTHORIZE_URL).map_err(|e| AuthError::Callback(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", ORIGINATOR);
    Ok(url.to_string())
}

/// Décode (sans vérifier la signature) la payload d'un JWT et en extrait
/// `chatgpt_account_id`. On ne vérifie pas la signature : on lit un claim, la
/// confiance vient du canal TLS d'OpenAI, pas d'une validation crypto locale.
pub fn extract_account_id(access_token: &str) -> Result<String, AuthError> {
    let payload = decode_jwt_payload(access_token)?;
    payload
        .get(JWT_CLAIM_NAMESPACE)
        .and_then(|ns| ns.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or(AuthError::MissingAccountId)
}

fn decode_jwt_payload(jwt: &str) -> Result<serde_json::Value, AuthError> {
    let payload_b64 = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| AuthError::Jwt("payload absente".to_string()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| AuthError::Jwt(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| AuthError::Jwt(e.to_string()))
}

fn token_to_credential(token: TokenResponse, now_ms: u64) -> Result<OAuthCredential, AuthError> {
    let account_id = extract_account_id(&token.access_token)?;
    Ok(OAuthCredential {
        provider: ProviderId::OpenAiChatGpt,
        access: Secret::new(token.access_token),
        refresh: Secret::new(token.refresh_token),
        // sliding : expires absolu = maintenant + expires_in (secondes → ms)
        expires_at: now_ms.saturating_add(token.expires_in.saturating_mul(1000)),
        account_id: Some(account_id),
    })
}

/// Résultat d'un callback browser.
#[derive(Clone, PartialEq, Eq)]
pub struct CallbackResult {
    pub code: String,
    pub state: String,
}

impl std::fmt::Debug for CallbackResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackResult")
            .field("code", &"Secret(***)")
            .field("state", &"Secret(***)")
            .finish()
    }
}

/// Parse la ligne de requête HTTP du callback (`GET /auth/callback?code=…&state=… HTTP/1.1`)
/// et valide le `state` (anti-CSRF).
pub fn parse_callback_request_line(
    line: &str,
    expected_state: &str,
) -> Result<CallbackResult, AuthError> {
    let target = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AuthError::Callback("ligne de requête HTTP invalide".to_string()))?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/auth/callback" {
        return Err(AuthError::Callback(format!("path inattendu : {path}")));
    }

    let mut code = None;
    let mut state = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }

    let state = state.ok_or_else(|| AuthError::Callback("state manquant".to_string()))?;
    if state != expected_state {
        return Err(AuthError::StateMismatch);
    }
    let code = code.ok_or_else(|| AuthError::Callback("code manquant".to_string()))?;
    Ok(CallbackResult { code, state })
}

/// Issue d'un poll device-code.
#[derive(Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Pending,
    SlowDown,
    Done {
        authorization_code: String,
        code_verifier: String,
    },
}

impl std::fmt::Debug for PollOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::SlowDown => f.write_str("SlowDown"),
            Self::Done { .. } => f
                .debug_struct("Done")
                .field("authorization_code", &"Secret(***)")
                .field("code_verifier", &"Secret(***)")
                .finish(),
        }
    }
}

fn device_error_code(body: &serde_json::Value) -> Option<&str> {
    body.get("errorCode")
        .or_else(|| body.get("error"))
        .and_then(|v| v.as_str())
}

/// Classifie une réponse de poll device-code (RFC 8628 + spécificités Codex).
pub fn classify_device_poll(
    status: u16,
    body: &serde_json::Value,
) -> Result<PollOutcome, AuthError> {
    if status == 200 {
        let code = body.get("authorization_code").and_then(|v| v.as_str());
        let verifier = body.get("code_verifier").and_then(|v| v.as_str());
        return match (code, verifier) {
            (Some(c), Some(v)) => Ok(PollOutcome::Done {
                authorization_code: c.to_string(),
                // ⚠️ en device flow, le code_verifier vient du SERVEUR, pas local.
                code_verifier: v.to_string(),
            }),
            _ => Err(AuthError::TokenResponse(
                "device 200 sans authorization_code/code_verifier".to_string(),
            )),
        };
    }
    match device_error_code(body) {
        Some("deviceauth_authorization_pending" | "authorization_pending") => {
            Ok(PollOutcome::Pending)
        }
        Some("slow_down") => Ok(PollOutcome::SlowDown),
        Some("expired_token") => Err(AuthError::DeviceTimeout),
        Some(other) => Err(AuthError::DeviceDenied(other.to_string())),
        None if status == 403 || status == 404 => Ok(PollOutcome::Pending),
        None => Err(AuthError::DeviceDenied(format!("http {status}"))),
    }
}

/// Spécification de requête d'inférence pour l'abonnement ChatGPT (backend
/// Responses API). À brancher dans l'adapter `agent-provider` (`OpenAiChatGpt`).
#[derive(Clone)]
pub struct RequestSpec {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for RequestSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                let value = if k.eq_ignore_ascii_case("authorization")
                    || k.eq_ignore_ascii_case("chatgpt-account-id")
                {
                    "Secret(***)"
                } else {
                    v.as_str()
                };
                (k.as_str(), value)
            })
            .collect();
        f.debug_struct("RequestSpec")
            .field("url", &self.url)
            .field("headers", &headers)
            .finish()
    }
}

/// En-têtes d'inférence SSE pour une credential abonnement ChatGPT. Le
/// `chatgpt-account-id` (dérivé du JWT) est requis pour router vers le compte.
pub fn responses_request(cred: &OAuthCredential) -> Result<RequestSpec, AuthError> {
    if cred.provider != ProviderId::OpenAiChatGpt {
        return Err(AuthError::WrongProvider(cred.provider));
    }
    let account_id = cred
        .account_id
        .as_deref()
        .ok_or(AuthError::MissingAccountId)?;
    Ok(RequestSpec {
        url: format!("{CHATGPT_BASE_URL}{RESPONSES_PATH}"),
        headers: vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", cred.access.expose()),
            ),
            ("chatgpt-account-id".to_string(), account_id.to_string()),
            ("originator".to_string(), originator()),
            ("OpenAI-Beta".to_string(), OPENAI_BETA_SSE.to_string()),
            ("accept".to_string(), "text/event-stream".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ],
    })
}

// ──────────────────────────── Réseau (token exchange / refresh) ────────────────────────────

/// Échange un `authorization_code` contre des tokens. `redirect_uri` diffère
/// entre browser (`REDIRECT_URI`) et device (`DEVICE_REDIRECT_URI`).
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    now_ms: u64,
) -> Result<OAuthCredential, AuthError> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await?
        .error_for_status()?;
    let token: TokenResponse = resp.json().await?;
    token_to_credential(token, now_ms)
}

/// Rafraîchit une credention via `grant_type=refresh_token`. Le refresh est
/// **rotatif** : la nouvelle credential porte un nouveau refresh à réécrire.
pub async fn refresh(
    client: &reqwest::Client,
    refresh_token: &str,
    now_ms: u64,
) -> Result<OAuthCredential, AuthError> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?
        .error_for_status()?;
    let token: TokenResponse = resp.json().await?;
    token_to_credential(token, now_ms)
}

// ──────────────────────────── Browser flow (PKCE + serveur callback local) ────────────────────────────

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Login interactif : ouvre le navigateur, attend le callback sur `127.0.0.1:1455`,
/// échange le code. L'ouverture du navigateur est best-effort — en cas d'échec,
/// l'URL est imprimée pour collage manuel.
pub async fn login_browser(client: &reqwest::Client) -> Result<OAuthCredential, AuthError> {
    let pkce = Pkce::generate();
    let state = random_state();
    let url = build_authorize_url(&pkce.challenge, &state)?;

    // bind AVANT d'ouvrir le navigateur (sinon course sur le callback)
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).await?;

    if open::that(&url).is_err() {
        println!("Ouvre cette URL pour autoriser Pyxis :\n{url}");
    }

    let cb = tokio::time::timeout(CALLBACK_TIMEOUT, accept_callback(&listener, &state))
        .await
        .map_err(|_| AuthError::Callback("callback OAuth expiré".to_string()))??;
    exchange_code(client, &cb.code, &pkce.verifier, REDIRECT_URI, now_ms()).await
}

const SUCCESS_BODY: &str = "<!doctype html><meta charset=utf-8><body style=\"font-family:system-ui;background:#0b0b0b;color:#eaeaea;display:grid;place-items:center;height:100vh\"><div><h2>Pyxis — connecté</h2><p>Tu peux fermer cet onglet.</p></div></body>";

/// Accepte des connexions jusqu'à recevoir un callback `/auth/callback` valide.
/// Les requêtes parasites (favicon, etc.) reçoivent un 404 et la boucle continue.
async fn accept_callback(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
) -> Result<CallbackResult, AuthError> {
    accept_callback_with_read_timeout(listener, expected_state, CALLBACK_READ_TIMEOUT).await
}

async fn accept_callback_with_read_timeout(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
    read_timeout: Duration,
) -> Result<CallbackResult, AuthError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = [0u8; 2048];
        let n = match tokio::time::timeout(read_timeout, sock.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                let _ = sock
                    .write_all(b"HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }
        };
        let req = String::from_utf8_lossy(&buf[..n]);
        let line = req.lines().next().unwrap_or("");

        match parse_callback_request_line(line, expected_state) {
            Ok(cb) => {
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    SUCCESS_BODY.len(),
                    SUCCESS_BODY
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                return Ok(cb);
            }
            // requête non pertinente → 404, on continue d'écouter
            Err(AuthError::Callback(_)) => {
                let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n").await;
            }
            // state mismatch (ou autre) → on coupe proprement
            Err(e) => {
                let _ = sock.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
                return Err(e);
            }
        }
    }
}

// ──────────────────────────── Device-code flow (headless) ────────────────────────────

/// Informations à présenter à l'utilisateur pour le device flow.
#[derive(Clone)]
pub struct DeviceAuth {
    pub user_code: String,
    pub verification_uri: String,
}

impl std::fmt::Debug for DeviceAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuth")
            .field("user_code", &"Secret(***)")
            .field("verification_uri", &self.verification_uri)
            .finish()
    }
}

/// État interne de poll (séparé de l'affichage utilisateur).
#[derive(Clone)]
pub struct DeviceAuthState {
    device_auth_id: String,
    user_code: String,
    interval: u64,
}

impl std::fmt::Debug for DeviceAuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuthState")
            .field("device_auth_id", &"Secret(***)")
            .field("user_code", &"Secret(***)")
            .field("interval", &self.interval)
            .finish()
    }
}

/// Démarre le device flow : retourne l'état à poller + les infos à afficher.
pub async fn start_device(
    client: &reqwest::Client,
) -> Result<(DeviceAuthState, DeviceAuth), AuthError> {
    let v: serde_json::Value = client
        .post(DEVICE_USER_CODE_URL)
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let device_auth_id = v
        .get("device_auth_id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AuthError::TokenResponse("device_auth_id absent".to_string()))?
        .to_string();
    let user_code = v
        .get("user_code")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AuthError::TokenResponse("user_code absent".to_string()))?
        .to_string();
    let interval = v
        .get("interval")
        .and_then(|x| x.as_u64())
        .unwrap_or(5)
        .max(1);

    let display = DeviceAuth {
        user_code: user_code.clone(),
        verification_uri: DEVICE_VERIFICATION_URI.to_string(),
    };
    Ok((
        DeviceAuthState {
            device_auth_id,
            user_code,
            interval,
        },
        display,
    ))
}

/// Poll jusqu'à autorisation, `slow_down`, ou timeout (900 s). Échange final via
/// `DEVICE_REDIRECT_URI`.
pub async fn poll_device(
    client: &reqwest::Client,
    st: &DeviceAuthState,
) -> Result<OAuthCredential, AuthError> {
    let start = tokio::time::Instant::now();
    let mut interval = st.interval;

    loop {
        if start.elapsed() >= DEVICE_CODE_TIMEOUT {
            return Err(AuthError::DeviceTimeout);
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;

        let resp = client
            .post(DEVICE_TOKEN_URL)
            .json(&serde_json::json!({
                "device_auth_id": st.device_auth_id,
                "user_code": st.user_code,
            }))
            .send()
            .await?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);

        match classify_device_poll(status, &body)? {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval = interval.saturating_add(5),
            PollOutcome::Done {
                authorization_code,
                code_verifier,
            } => {
                return exchange_code(
                    client,
                    &authorization_code,
                    &code_verifier,
                    DEVICE_REDIRECT_URI,
                    now_ms(),
                )
                .await;
            }
        }
    }
}

// ──────────────────────────── Horloge ────────────────────────────

/// Maintenant en ms epoch (source de `expires_at`).
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ──────────────────────────────── Tests ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn extract_account_id_reads_custom_claim() {
        let jwt = make_jwt(&serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_42" }
        }));
        assert_eq!(extract_account_id(&jwt).unwrap(), "acct_42");
    }

    #[test]
    fn extract_account_id_missing_claim_errors() {
        let jwt = make_jwt(&serde_json::json!({ "sub": "user_1" }));
        assert!(matches!(
            extract_account_id(&jwt),
            Err(AuthError::MissingAccountId)
        ));
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let url = build_authorize_url("CHAL", "STATE123").unwrap();
        for needle in [
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
            "code_challenge=CHAL",
            "code_challenge_method=S256",
            "state=STATE123",
            "id_token_add_organizations=true",
            "codex_cli_simplified_flow=true",
            "originator=pyxis",
            "scope=openid",
        ] {
            assert!(url.contains(needle), "param absent: {needle}\n{url}");
        }
        // redirect_uri encodé
        assert!(url.contains("redirect_uri=http"));
    }

    #[test]
    fn callback_parses_code_and_validates_state() {
        let line = "GET /auth/callback?code=abc123&state=s1 HTTP/1.1";
        let cb = parse_callback_request_line(line, "s1").unwrap();
        assert_eq!(cb.code, "abc123");

        assert!(matches!(
            parse_callback_request_line("GET /auth/callback2?code=abc123&state=s1 HTTP/1.1", "s1"),
            Err(AuthError::Callback(_))
        ));
        assert!(matches!(
            parse_callback_request_line(
                "GET /auth/callback/extra?code=abc123&state=s1 HTTP/1.1",
                "s1"
            ),
            Err(AuthError::Callback(_))
        ));
        // mauvais state → CSRF
        assert!(matches!(
            parse_callback_request_line(line, "WRONG"),
            Err(AuthError::StateMismatch)
        ));
        // requête parasite → erreur Callback (la boucle 404 et continue)
        assert!(matches!(
            parse_callback_request_line("GET /favicon.ico HTTP/1.1", "s1"),
            Err(AuthError::Callback(_))
        ));
    }

    #[test]
    fn device_poll_classification() {
        let null = serde_json::Value::Null;
        assert_eq!(
            classify_device_poll(403, &null).unwrap(),
            PollOutcome::Pending
        );
        assert_eq!(
            classify_device_poll(404, &null).unwrap(),
            PollOutcome::Pending
        );
        assert_eq!(
            classify_device_poll(
                400,
                &serde_json::json!({"errorCode":"deviceauth_authorization_pending"})
            )
            .unwrap(),
            PollOutcome::Pending
        );
        assert_eq!(
            classify_device_poll(400, &serde_json::json!({"errorCode":"slow_down"})).unwrap(),
            PollOutcome::SlowDown
        );
        assert_eq!(
            classify_device_poll(400, &serde_json::json!({"error":"authorization_pending"}))
                .unwrap(),
            PollOutcome::Pending
        );
        assert!(matches!(
            classify_device_poll(403, &serde_json::json!({"errorCode":"access_denied"})),
            Err(AuthError::DeviceDenied(e)) if e == "access_denied"
        ));
        assert!(matches!(
            classify_device_poll(404, &serde_json::json!({"error":"expired_token"})),
            Err(AuthError::DeviceTimeout)
        ));
        let done = classify_device_poll(
            200,
            &serde_json::json!({"authorization_code":"C","code_verifier":"V"}),
        )
        .unwrap();
        assert_eq!(
            done,
            PollOutcome::Done {
                authorization_code: "C".into(),
                code_verifier: "V".into()
            }
        );
        assert!(matches!(
            classify_device_poll(400, &serde_json::json!({"errorCode":"access_denied"})),
            Err(AuthError::DeviceDenied(_))
        ));
        assert!(matches!(
            classify_device_poll(200, &serde_json::json!({"authorization_code":"C"})),
            Err(AuthError::TokenResponse(_))
        ));
    }

    #[test]
    fn token_to_credential_sets_provider_and_sliding_expiry() {
        let jwt = make_jwt(&serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_9" }
        }));
        let token = TokenResponse {
            access_token: jwt,
            refresh_token: "rt".to_string(),
            expires_in: 3600,
        };
        let cred = token_to_credential(token, 1_000).unwrap();
        assert_eq!(cred.provider, ProviderId::OpenAiChatGpt);
        assert_eq!(cred.account_id.as_deref(), Some("acct_9"));
        assert_eq!(cred.expires_at, 1_000 + 3_600_000);
        assert!(!cred.is_expired(cred.expires_at - 1));
        assert!(cred.is_expired(cred.expires_at));
    }

    #[test]
    fn responses_request_has_proprietary_headers() {
        let cred = OAuthCredential {
            provider: ProviderId::OpenAiChatGpt,
            access: Secret::new("AT"),
            refresh: Secret::new("RT"),
            expires_at: 0,
            account_id: Some("acct_7".into()),
        };
        let spec = responses_request(&cred).unwrap();
        assert_eq!(spec.url, "https://chatgpt.com/backend-api/codex/responses");
        let h: std::collections::HashMap<_, _> = spec.headers.into_iter().collect();
        assert_eq!(h["Authorization"], "Bearer AT");
        assert_eq!(h["chatgpt-account-id"], "acct_7");
        assert_eq!(h["originator"], "pyxis");
        assert_eq!(h["OpenAI-Beta"], "responses=experimental");
    }

    #[test]
    fn responses_request_rejects_wrong_provider() {
        let cred = OAuthCredential {
            provider: ProviderId::Anthropic,
            access: Secret::new("AT"),
            refresh: Secret::new("RT"),
            expires_at: 0,
            account_id: Some("acct_7".into()),
        };
        assert!(matches!(
            responses_request(&cred),
            Err(AuthError::WrongProvider(ProviderId::Anthropic))
        ));
    }

    #[test]
    fn debug_output_redacts_oauth_transients() {
        let cred = OAuthCredential {
            provider: ProviderId::OpenAiChatGpt,
            access: Secret::new("AT_SECRET"),
            refresh: Secret::new("RT_SECRET"),
            expires_at: 0,
            account_id: Some("acct_7".into()),
        };
        let spec = responses_request(&cred).unwrap();
        let spec_dbg = format!("{spec:?}");
        assert!(!spec_dbg.contains("AT_SECRET"));
        assert!(!spec_dbg.contains("acct_7"));
        assert!(spec_dbg.contains("Secret(***)"));

        let cb = CallbackResult {
            code: "CODE_SECRET".into(),
            state: "STATE_SECRET".into(),
        };
        let cb_dbg = format!("{cb:?}");
        assert!(!cb_dbg.contains("CODE_SECRET"));
        assert!(!cb_dbg.contains("STATE_SECRET"));

        let done = PollOutcome::Done {
            authorization_code: "AUTH_CODE_SECRET".into(),
            code_verifier: "VERIFIER_SECRET".into(),
        };
        let done_dbg = format!("{done:?}");
        assert!(!done_dbg.contains("AUTH_CODE_SECRET"));
        assert!(!done_dbg.contains("VERIFIER_SECRET"));

        let st = DeviceAuthState {
            device_auth_id: "DEVICE_SECRET".into(),
            user_code: "USER_SECRET".into(),
            interval: 5,
        };
        let st_dbg = format!("{st:?}");
        assert!(!st_dbg.contains("DEVICE_SECRET"));
        assert!(!st_dbg.contains("USER_SECRET"));

        let display = DeviceAuth {
            user_code: "DISPLAY_CODE_SECRET".into(),
            verification_uri: DEVICE_VERIFICATION_URI.into(),
        };
        let display_dbg = format!("{display:?}");
        assert!(!display_dbg.contains("DISPLAY_CODE_SECRET"));
    }

    #[tokio::test]
    async fn callback_read_timeout_ignores_silent_socket() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            accept_callback_with_read_timeout(&listener, "s1", Duration::from_millis(20)).await
        });

        let _silent = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut good = tokio::net::TcpStream::connect(addr).await.unwrap();
        good.write_all(b"GET /auth/callback?code=abc123&state=s1 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();

        let cb = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(cb.code, "abc123");
    }

    // US-021 AC2 : sélection du fallback `originator`. `pyxis` par défaut ;
    // `codex_cli_rs` si le backend rejette `pyxis` (à trancher en live).
    #[test]
    fn originator_fallback_selection() {
        assert_eq!(originator_for(true), "pyxis");
        assert_eq!(originator_for(false), "codex_cli_rs");
        assert_eq!(ORIGINATOR_FALLBACK, "codex_cli_rs");
        // env non défini → défaut `pyxis` (le run live le surchargera si besoin).
        assert_eq!(originator(), "pyxis");
    }

    #[test]
    fn responses_request_without_account_id_errors() {
        let cred = OAuthCredential {
            provider: ProviderId::OpenAiChatGpt,
            access: Secret::new("AT"),
            refresh: Secret::new("RT"),
            expires_at: 0,
            account_id: None,
        };
        assert!(matches!(
            responses_request(&cred),
            Err(AuthError::MissingAccountId)
        ));
    }
}
