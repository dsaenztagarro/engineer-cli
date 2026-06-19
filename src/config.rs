use color_eyre::eyre::{eyre, Context, Result};
use directories::BaseDirs;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use url::Url;

/// Built-in URL presets. The target environment is chosen at runtime (see
/// `Environment`); there is no separate dev/prod build.
const PROD_IDENTITY_URL: &str = "https://identity.dsaenz.dev";
const PROD_API_URL: &str = "https://engineer.dsaenz.dev";
const DEV_IDENTITY_URL: &str = "http://localhost:4000";
const DEV_API_URL: &str = "http://localhost:4001";

/// Path, relative to the api host, where the Engineer app serves engineer-tui's
/// OAuth Client ID Metadata Document. The full URL is this client's `client_id`.
const CIMD_PATH: &str = ".well-known/oauth-client/engineer-tui.json";

/// Which set of servers to talk to. Selected explicitly via `--env` /
/// `ENGINEER_ENV`; defaults to production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Production,
    Development,
}

impl FromStr for Environment {
    type Err = color_eyre::eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "production" | "prod" => Ok(Self::Production),
            "development" | "dev" | "local" => Ok(Self::Development),
            other => Err(eyre!(
                "unknown environment {other:?} — use `production` or `development`"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub identity_url: Url,
    pub api_url: Url,
    /// Optional explicit client_id. When unset, it is derived from `api_url` as
    /// the CIMD document URL (see `client_id`).
    pub client_id: Option<String>,
    pub scopes: String,
}

/// Partial, file-shaped config: every field optional so `config.toml` overrides
/// only the keys it sets, leaving the rest of the chosen environment's preset.
///
/// `deny_unknown_fields` makes a typo (e.g. the `ENGINEER_API_URL` env-var name
/// used as a key instead of `api_url`) a hard error at load, rather than being
/// silently ignored and falling back to the production preset.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    identity_url: Option<Url>,
    api_url: Option<Url>,
    client_id: Option<String>,
    scopes: Option<String>,
}

fn default_scopes() -> String {
    "read write".into()
}

impl Default for Config {
    fn default() -> Self {
        Self::for_environment(Environment::Production)
    }
}

impl Config {
    /// Baseline config for an environment, before file/env overrides.
    pub fn for_environment(env: Environment) -> Self {
        let (identity, api) = match env {
            Environment::Production => (PROD_IDENTITY_URL, PROD_API_URL),
            Environment::Development => (DEV_IDENTITY_URL, DEV_API_URL),
        };
        Self {
            identity_url: Url::parse(identity).expect("valid preset identity_url"),
            api_url: Url::parse(api).expect("valid preset api_url"),
            client_id: None,
            scopes: default_scopes(),
        }
    }

    /// Layered config: environment preset < `config.toml` (if present) < env vars.
    /// A missing config file is not an error — the preset plus CIMD make a fresh
    /// run zero-config.
    pub fn load(env: Environment) -> Result<Self> {
        let mut cfg = Self::for_environment(env);

        let path = Self::path()?;
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let file: FileConfig =
                toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
            if let Some(v) = file.identity_url {
                cfg.identity_url = v;
            }
            if let Some(v) = file.api_url {
                cfg.api_url = v;
            }
            if let Some(v) = file.client_id {
                cfg.client_id = Some(v);
            }
            if let Some(v) = file.scopes {
                cfg.scopes = v;
            }
        }

        cfg.apply_env_overrides()?;
        Ok(cfg)
    }

    fn apply_env_overrides(&mut self) -> Result<()> {
        if let Ok(v) = std::env::var("ENGINEER_IDENTITY_URL") {
            self.identity_url =
                Url::parse(&v).with_context(|| format!("ENGINEER_IDENTITY_URL: invalid URL {v:?}"))?;
        }
        if let Ok(v) = std::env::var("ENGINEER_API_URL") {
            self.api_url =
                Url::parse(&v).with_context(|| format!("ENGINEER_API_URL: invalid URL {v:?}"))?;
        }
        if let Ok(v) = std::env::var("ENGINEER_CLIENT_ID") {
            self.client_id = Some(v);
        }
        if let Ok(v) = std::env::var("ENGINEER_SCOPES") {
            self.scopes = v;
        }
        Ok(())
    }

    /// Resolve the config file path. Honors `XDG_CONFIG_HOME`, otherwise
    /// `~/.config/engineer-tui/config.toml` on every platform — notably also on
    /// macOS, where the platform-native dir would be buried under
    /// `~/Library/Application Support`.
    pub fn path() -> Result<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("engineer-tui").join("config.toml"));
            }
        }
        let base = BaseDirs::new().ok_or_else(|| eyre!("could not resolve home directory"))?;
        Ok(base
            .home_dir()
            .join(".config")
            .join("engineer-tui")
            .join("config.toml"))
    }

    /// Directory for application logs (including the API-communication log).
    /// Honors `XDG_STATE_HOME`, otherwise `~/.local/state/engineer-tui/` on every
    /// platform — mirroring `path()`'s XDG-everywhere policy so the log location
    /// is predictable for `tail -f` even on macOS.
    pub fn log_dir() -> Result<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("engineer-tui"));
            }
        }
        let base = BaseDirs::new().ok_or_else(|| eyre!("could not resolve home directory"))?;
        Ok(base.home_dir().join(".local").join("state").join("engineer-tui"))
    }

    /// The OAuth `client_id`. Returns the explicit value if configured, else the
    /// CIMD document URL derived from `api_url` — Identity fetches that URL to
    /// resolve the client, so it doubles as the client identity. Keeping it
    /// derived means dev (localhost) and prod stay in sync with `api_url`.
    pub fn client_id(&self) -> String {
        if let Some(id) = &self.client_id {
            if !id.is_empty() {
                return id.clone();
            }
        }
        self.api_url
            .join(CIMD_PATH)
            .map(String::from)
            .unwrap_or_else(|_| format!("{}{CIMD_PATH}", self.api_url))
    }

    /// Identity host string used as the keyring account name. Stable across ports.
    pub fn keyring_account(&self) -> String {
        self.identity_url.as_str().trim_end_matches('/').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_preset_uses_dsaenz_hosts() {
        let cfg = Config::for_environment(Environment::Production);
        assert_eq!(cfg.identity_url.as_str(), "https://identity.dsaenz.dev/");
        assert_eq!(cfg.api_url.as_str(), "https://engineer.dsaenz.dev/");
    }

    #[test]
    fn development_preset_uses_localhost() {
        let cfg = Config::for_environment(Environment::Development);
        assert_eq!(cfg.identity_url.as_str(), "http://localhost:4000/");
        assert_eq!(cfg.api_url.as_str(), "http://localhost:4001/");
    }

    #[test]
    fn environment_parses_aliases() {
        assert_eq!("prod".parse::<Environment>().unwrap(), Environment::Production);
        assert_eq!("dev".parse::<Environment>().unwrap(), Environment::Development);
        assert_eq!("local".parse::<Environment>().unwrap(), Environment::Development);
        assert!("staging".parse::<Environment>().is_err());
    }

    #[test]
    fn derives_cimd_client_id_from_prod_api_url() {
        let cfg = Config::for_environment(Environment::Production);
        // Must equal the URL the Engineer app serves and Identity fetches.
        assert_eq!(
            cfg.client_id(),
            "https://engineer.dsaenz.dev/.well-known/oauth-client/engineer-tui.json"
        );
    }

    #[test]
    fn derives_cimd_client_id_for_localhost_dev() {
        let cfg = Config::for_environment(Environment::Development);
        assert_eq!(
            cfg.client_id(),
            "http://localhost:4001/.well-known/oauth-client/engineer-tui.json"
        );
    }

    #[test]
    fn explicit_client_id_overrides_derivation() {
        let mut cfg = Config::default();
        cfg.client_id = Some("https://example.test/cid.json".into());
        assert_eq!(cfg.client_id(), "https://example.test/cid.json");
    }

    #[test]
    fn empty_explicit_client_id_falls_back_to_derivation() {
        let mut cfg = Config::default();
        cfg.client_id = Some(String::new());
        assert_eq!(
            cfg.client_id(),
            "https://engineer.dsaenz.dev/.well-known/oauth-client/engineer-tui.json"
        );
    }

    #[test]
    fn file_config_rejects_unknown_keys() {
        // The env-var name used as a key (a common mistake) must error, not be
        // silently ignored.
        let err = toml::from_str::<FileConfig>(r#"ENGINEER_API_URL = "http://localhost:4001""#);
        assert!(err.is_err());
    }

    #[test]
    fn file_config_accepts_known_keys() {
        let fc: FileConfig = toml::from_str(
            "identity_url = \"http://localhost:4000\"\napi_url = \"http://localhost:4001\"",
        )
        .expect("known keys parse");
        assert_eq!(fc.api_url.unwrap().as_str(), "http://localhost:4001/");
        assert_eq!(fc.identity_url.unwrap().as_str(), "http://localhost:4000/");
    }
}
