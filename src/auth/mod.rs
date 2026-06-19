//! OAuth2 (RFC 6749) Authorization Code + PKCE (RFC 7636) flow over a loopback
//! redirect URI (RFC 8252) against the Identity server. Refresh tokens live in
//! the OS keyring; access tokens stay in memory only.

use color_eyre::eyre::{eyre, Result};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::Config;

mod oauth;
mod storage;

pub use oauth::{discover, login, logout, refresh, Discovery};

/// Provides a valid access token to API calls, refreshing transparently.
#[derive(Clone)]
pub struct TokenProvider {
    inner: Arc<Mutex<State>>,
    config: Config,
    discovery: Discovery,
}

struct State {
    access: Option<String>,
    expires_at: Option<jiff::Timestamp>,
}

impl TokenProvider {
    pub async fn new(config: Config) -> Result<Self> {
        let discovery = discover(&config).await?;
        Ok(Self {
            inner: Arc::new(Mutex::new(State { access: None, expires_at: None })),
            config,
            discovery,
        })
    }

    /// Returns a non-expired access token, refreshing via the stored refresh token if needed.
    pub async fn access_token(&self) -> Result<String> {
        {
            let s = self.inner.lock().await;
            if let (Some(tok), Some(exp)) = (&s.access, s.expires_at) {
                if exp > jiff::Timestamp::now() + jiff::SignedDuration::from_secs(30) {
                    return Ok(tok.clone());
                }
            }
        }
        let refresh_token = storage::load(&self.config.keyring_account())?
            .ok_or_else(|| eyre!("not logged in — run `engineer login`"))?;
        let issued = refresh(&self.config, &self.discovery, &refresh_token).await?;
        let mut s = self.inner.lock().await;
        s.access = Some(issued.access.clone());
        s.expires_at = issued.expires_at;
        if let Some(new_refresh) = issued.refresh {
            storage::store(&self.config.keyring_account(), &new_refresh)?;
        }
        Ok(issued.access)
    }
}

/// Whether a refresh token is present in the keyring (used at TUI startup to
/// pick the Login vs Home screen). Keyring errors are treated as "not logged in".
pub fn is_logged_in(cfg: &Config) -> bool {
    matches!(storage::load(&cfg.keyring_account()), Ok(Some(_)))
}

/// Persist a refresh token. Exposed so the TUI login flow can store the token
/// without reaching into the private `storage` module.
pub fn store_refresh(cfg: &Config, refresh_token: &str) -> Result<()> {
    storage::store(&cfg.keyring_account(), refresh_token)
}

pub async fn login_cli(cfg: &Config) -> Result<()> {
    let discovery = discover(cfg).await?;
    let issued = login(cfg, &discovery, true).await?;
    if let Some(refresh) = &issued.refresh {
        storage::store(&cfg.keyring_account(), refresh)?;
    }
    println!("Logged in. Refresh token stored in OS keyring.");
    let me = crate::api::ApiClient::with_token(cfg.api_url.clone(), issued.access.clone())
        .me()
        .await?;
    println!("  user: {} <{}>", me.name.unwrap_or_default(), me.email);
    Ok(())
}

pub async fn logout_cli(cfg: &Config) -> Result<()> {
    let account = cfg.keyring_account();
    if let Some(refresh_token) = storage::load(&account)? {
        let discovery = discover(cfg).await?;
        let _ = logout(cfg, &discovery, &refresh_token).await;
    }
    storage::delete(&account)?;
    println!("Logged out.");
    Ok(())
}

pub async fn whoami_cli(cfg: &Config) -> Result<()> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let me = crate::api::ApiClient::with_token(cfg.api_url.clone(), token).me().await?;
    println!("{} <{}> (id: {})", me.name.unwrap_or_default(), me.email, me.id);
    Ok(())
}
