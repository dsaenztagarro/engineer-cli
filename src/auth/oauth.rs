//! Authorization Code + PKCE (RFC 7636) over a loopback redirect (RFC 8252).
//!
//! Flow: bind 127.0.0.1:0 → build authorize URL → open browser →
//! receive callback → exchange code → return tokens.

use color_eyre::eyre::{eyre, Context, Result};
use serde::Deserialize;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::timeout;
use url::Url;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct Discovery {
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub revocation_endpoint: Option<Url>,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDoc {
    authorization_endpoint: Url,
    token_endpoint: Url,
    revocation_endpoint: Option<Url>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    token_type: Option<String>,
}

pub struct IssuedTokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: Option<jiff::Timestamp>,
}

pub async fn discover(cfg: &Config) -> Result<Discovery> {
    let url = cfg
        .identity_url
        .join("/.well-known/oauth-authorization-server")?;
    let doc: DiscoveryDoc = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(Discovery {
        authorization_endpoint: doc.authorization_endpoint,
        token_endpoint: doc.token_endpoint,
        revocation_endpoint: doc.revocation_endpoint,
    })
}

/// Run the Authorization Code + PKCE flow. `cli` controls user feedback: the
/// `login` subcommand (true) prints the authorize URL to stdout as a fallback;
/// the TUI (false) must not write to stdout — it owns the alternate screen — so
/// the URL goes to the log instead.
pub async fn login(cfg: &Config, discovery: &Discovery, cli: bool) -> Result<IssuedTokens> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind loopback")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let (verifier, challenge) = pkce_pair();
    let state = random_token(32);
    let client_id = cfg.client_id();

    let mut authz = discovery.authorization_endpoint.clone();
    authz
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", &cfg.scopes)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    if cli {
        println!("Opening browser to: {authz}");
    } else {
        tracing::info!(url = %authz, "opening browser for OAuth login");
    }
    if open::that(authz.as_str()).is_err() && cli {
        println!("(could not open browser automatically — paste the URL above)");
    }

    let (code, recv_state) = timeout(Duration::from_secs(300), accept_callback(listener))
        .await
        .map_err(|_| eyre!("timed out waiting for OAuth callback"))??;

    if recv_state != state {
        return Err(eyre!("state mismatch — possible CSRF; aborting"));
    }

    let resp: TokenResponse = reqwest::Client::new()
        .post(discovery.token_endpoint.clone())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("client_id", &client_id),
            ("code_verifier", &verifier),
        ])
        .send()
        .await?
        .error_for_status()
        .context("token exchange failed")?
        .json()
        .await?;

    Ok(IssuedTokens {
        access: resp.access_token,
        refresh: resp.refresh_token,
        expires_at: resp
            .expires_in
            .map(|s| jiff::Timestamp::now() + jiff::SignedDuration::from_secs(s)),
    })
}

pub async fn refresh(
    cfg: &Config,
    discovery: &Discovery,
    refresh_token: &str,
) -> Result<IssuedTokens> {
    let client_id = cfg.client_id();
    let resp: TokenResponse = reqwest::Client::new()
        .post(discovery.token_endpoint.clone())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &client_id),
        ])
        .send()
        .await?
        .error_for_status()
        .context("refresh failed — refresh token may be revoked, run `engineer login`")?
        .json()
        .await?;

    Ok(IssuedTokens {
        access: resp.access_token,
        refresh: resp.refresh_token,
        expires_at: resp
            .expires_in
            .map(|s| jiff::Timestamp::now() + jiff::SignedDuration::from_secs(s)),
    })
}

pub async fn logout(cfg: &Config, discovery: &Discovery, refresh_token: &str) -> Result<()> {
    let Some(endpoint) = &discovery.revocation_endpoint else {
        return Ok(());
    };
    let client_id = cfg.client_id();
    reqwest::Client::new()
        .post(endpoint.clone())
        .form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", &client_id),
        ])
        .send()
        .await?;
    Ok(())
}

async fn accept_callback(listener: TcpListener) -> Result<(String, String)> {
    let (mut socket, _) = listener.accept().await?;
    let peer: SocketAddr = socket.peer_addr()?;
    tracing::debug!(?peer, "received OAuth callback");

    let mut reader = BufReader::new(&mut socket);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| eyre!("malformed HTTP request"))?
        .to_string();

    let url = Url::parse(&format!("http://localhost{path}"))?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in url.query_pairs() {
        match &*k {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }

    let body = if let Some(ref err) = error {
        format!(
            "<html><body style='font-family:system-ui;padding:3rem;'><h2>Authorization failed</h2><p>{err}</p></body></html>"
        )
    } else {
        "<html><body style='font-family:system-ui;padding:3rem;'><h2>Engineer TUI</h2><p>You can return to your terminal.</p></body></html>".to_string()
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await.ok();

    if let Some(err) = error {
        return Err(eyre!("authorization server returned error: {err}"));
    }
    Ok((
        code.ok_or_else(|| eyre!("no `code` in callback"))?,
        state.ok_or_else(|| eyre!("no `state` in callback"))?,
    ))
}

fn pkce_pair() -> (String, String) {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use sha2::{Digest, Sha256};

    let verifier = random_token(64);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

fn random_token(bytes: usize) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pair_is_well_formed() {
        let (verifier, challenge) = pkce_pair();
        // RFC 7636: verifier 43-128 unreserved chars; we use base64url(64 bytes) = 86 chars.
        assert!((43..=128).contains(&verifier.len()));
        // SHA256 → 32 bytes → base64url no-pad = 43 chars.
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
    }
}
