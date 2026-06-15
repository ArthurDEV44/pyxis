//! Proxy CONNECT local avec allow-list de hostnames (filtrage réseau best-effort
//! applicatif — Landlock ne sait pas le faire, cf. ADR-7 R3).
//!
//! `demo()` est auto-contenu et déterministe (aucun accès Internet requis) :
//! il monte un upstream TCP local, route un hôte autorisé à travers le proxy
//! (tunnel établi) et un hôte interdit (403 + journalisation), puis asserte les
//! deux issues. La résolution DNS est stubbée via `resolve` pour la repro ; un
//! vrai proxy résoudrait via le DNS système — la logique de sécurité (le check
//! d'allow-list sur le hostname demandé) est, elle, identique.

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct ProxyConfig {
    /// Hostnames explicitement autorisés (fail-closed : tout le reste est bloqué).
    pub allow: Vec<String>,
    /// host -> addr concrète. DNS stubbé pour la démo auto-contenue.
    pub resolve: HashMap<String, String>,
}

impl ProxyConfig {
    fn is_allowed(&self, host: &str) -> bool {
        self.allow.iter().any(|h| h == host)
    }
}

/// Traite une connexion cliente : lit la requête CONNECT, applique l'allow-list,
/// tunnelise si autorisé, renvoie 403 sinon.
async fn handle_conn(mut client: TcpStream, cfg: Arc<ProxyConfig>) -> Result<()> {
    // Lire jusqu'à la fin des en-têtes (CRLFCRLF).
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = client.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            bail!("requête proxy anormalement longue");
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or(""); // host:port

    if method != "CONNECT" {
        client
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
            .await?;
        return Ok(());
    }

    let host = target.split(':').next().unwrap_or(target).to_string();
    let port = target.split(':').nth(1).unwrap_or("443").to_string();

    if !cfg.is_allowed(&host) {
        eprintln!(
            "[proxy] BLOQUÉ  host={host} (hors allow-list {:?}) — 403",
            cfg.allow
        );
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\n\r\nblocked by numen allow-list")
            .await?;
        return Ok(());
    }

    let upstream_addr = cfg
        .resolve
        .get(&host)
        .cloned()
        .unwrap_or_else(|| format!("{host}:{port}"));
    eprintln!("[proxy] AUTORISÉ host={host} -> {upstream_addr} — tunnel établi");

    let mut upstream = TcpStream::connect(&upstream_addr).await?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Petit upstream TCP : annonce une bannière connue dès qu'un client se connecte.
async fn spawn_upstream() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let _ = sock.write_all(b"UPSTREAM-OK\n").await;
            let _ = sock.flush().await;
        }
    });
    Ok(addr)
}

async fn spawn_proxy(cfg: Arc<ProxyConfig>) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let cfg = Arc::clone(&cfg);
            tokio::spawn(async move {
                if let Err(e) = handle_conn(sock, cfg).await {
                    eprintln!("[proxy] conn error: {e}");
                }
            });
        }
    });
    Ok(addr)
}

/// Envoie une requête CONNECT au proxy et retourne (ligne de statut, corps reçu).
async fn connect_through(proxy_addr: &str, target_host: &str) -> Result<(String, String)> {
    let mut s = TcpStream::connect(proxy_addr).await?;
    let req = format!("CONNECT {target_host}:443 HTTP/1.1\r\nHost: {target_host}\r\n\r\n");
    s.write_all(req.as_bytes()).await?;

    let mut out = Vec::new();
    let mut tmp = [0u8; 512];
    // Quelques lectures suffisent pour la démo (status + éventuelle bannière upstream).
    for _ in 0..4 {
        match tokio::time::timeout(std::time::Duration::from_millis(400), s.read(&mut tmp)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => out.extend_from_slice(&tmp[..n]),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => break, // timeout : on a lu ce qu'il y avait
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    let status = text.lines().next().unwrap_or("").to_string();
    Ok((status, text))
}

pub async fn demo() -> Result<()> {
    let upstream = spawn_upstream().await?;
    let mut resolve = HashMap::new();
    resolve.insert("api.allowed.test".to_string(), upstream.clone());

    let cfg = Arc::new(ProxyConfig {
        allow: vec!["api.allowed.test".to_string()],
        resolve,
    });
    let proxy_addr = spawn_proxy(Arc::clone(&cfg)).await?;
    println!(
        "[proxy] proxy={proxy_addr}  upstream={upstream}  allow={:?}",
        cfg.allow
    );

    // Cas 1 : hôte autorisé → tunnel établi + bannière upstream.
    let (status_ok, body_ok) = connect_through(&proxy_addr, "api.allowed.test").await?;
    println!("[proxy] autorisé  -> status={status_ok:?}");
    if !status_ok.contains("200") {
        bail!("hôte autorisé non tunnelisé : {status_ok:?}");
    }
    if !body_ok.contains("UPSTREAM-OK") {
        bail!("tunnel établi mais bannière upstream absente — copie bidirectionnelle KO");
    }

    // Cas 2 : hôte interdit → 403, jamais de connexion upstream (unhappy path).
    let (status_blocked, _) = connect_through(&proxy_addr, "evil.exfil.test").await?;
    println!("[proxy] interdit  -> status={status_blocked:?}");
    if !status_blocked.contains("403") {
        bail!("hôte interdit NON bloqué : {status_blocked:?} — allow-list inopérante");
    }

    println!(
        "[proxy] VERDICT: filtrage réseau par allow-list faisable en solo (proxy CONNECT applicatif)."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allowed_host_tunnels_and_blocked_host_403() {
        let upstream = spawn_upstream().await.unwrap();
        let mut resolve = HashMap::new();
        resolve.insert("api.allowed.test".to_string(), upstream);
        let cfg = Arc::new(ProxyConfig {
            allow: vec!["api.allowed.test".to_string()],
            resolve,
        });
        let proxy_addr = spawn_proxy(cfg).await.unwrap();

        let (ok, body) = connect_through(&proxy_addr, "api.allowed.test")
            .await
            .unwrap();
        assert!(ok.contains("200"), "status autorisé inattendu: {ok}");
        assert!(body.contains("UPSTREAM-OK"), "bannière upstream absente");

        let (blocked, _) = connect_through(&proxy_addr, "evil.exfil.test")
            .await
            .unwrap();
        assert!(
            blocked.contains("403"),
            "hôte interdit non bloqué: {blocked}"
        );
    }

    #[test]
    fn allowlist_is_fail_closed() {
        let cfg = ProxyConfig {
            allow: vec!["good.test".to_string()],
            resolve: HashMap::new(),
        };
        assert!(cfg.is_allowed("good.test"));
        assert!(!cfg.is_allowed("evil.test"));
        assert!(!cfg.is_allowed("good.test.evil.test"));
    }
}
