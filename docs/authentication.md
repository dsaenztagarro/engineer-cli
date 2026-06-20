# Authentication

`engineer-cli` authenticates against the Engineer **Identity** server using
OAuth2 Authorization Code with PKCE, over a loopback redirect. Refresh tokens
live in the OS keyring; access tokens stay in memory and are refreshed
transparently. The relevant code is in `src/auth/`.

## Standards

| Concern | Standard | Where |
|---------|----------|-------|
| Authorization grant | OAuth2 Authorization Code (RFC 6749) | `src/auth/oauth.rs` |
| Public-client protection | PKCE (RFC 7636), `S256` | `pkce_pair` in `oauth.rs` |
| Native-app redirect | Loopback redirect URI (RFC 8252), `http://127.0.0.1:<ephemeral>/callback` | `oauth.rs` |
| Server metadata | `.well-known/oauth-authorization-server` discovery | `discover` in `oauth.rs` |
| Token introspection | Server-side (RFC 7662) — the API validates the Bearer token | backend |

The client id is the **Client ID Metadata Document (CIMD)** URL derived from the
API host (`Config::client_id`, e.g.
`https://engineer.dsaenz.dev/.well-known/oauth-client/engineer-cli.json`), so dev
and prod stay in sync with `api_url`. Scopes default to `read write`
(`Config::scopes`).

## TokenProvider

`TokenProvider` (`src/auth/mod.rs`) is the single source of access tokens for the
API client. It caches the access token + expiry in memory and refreshes lazily:

```rust
pub async fn access_token(&self) -> Result<String> {
    // 1. return the cached token if it's valid for >30s more
    // 2. otherwise load the refresh token from the keyring
    // 3. exchange it at the token endpoint (refresh grant)
    // 4. cache the new access token; persist a rotated refresh token
}
```

The 30-second skew avoids handing out a token that expires mid-request. Token
rotation is handled: if the refresh response includes a new refresh token, it is
written back to the keyring.

## Token storage

`src/auth/storage.rs` wraps the [`keyring`] crate:

- service name `"engineer-cli"`, account = the identity host
  (`Config::keyring_account`, stable across ports);
- the **refresh token** is the only persisted secret;
- the **access token never touches disk** — it lives only in `TokenProvider`'s
  in-memory `State`.

`load`/`store`/`delete` treat a missing entry as "not logged in" rather than an
error.

## Startup gate

At launch, `run_loop` (`src/app/mod.rs`) calls `auth::is_logged_in` (a keyring
probe) to pick the initial screen: **Login** when there is no stored refresh
token, otherwise **Home** with an immediate `FetchMe`.

## TUI login flow

```mermaid
sequenceDiagram
    participant U as User
    participant APP as App::handle
    participant OA as auth::oauth
    participant BR as Browser
    participant KR as keyring
    participant API as ApiClient

    U->>APP: Enter (Action::Login)
    APP->>OA: discover() + login() (spawned)
    OA->>BR: open authorize URL (PKCE challenge)
    BR-->>OA: redirect to 127.0.0.1/callback?code=…
    OA->>OA: exchange code + verifier at token endpoint
    OA->>KR: store_refresh(refresh_token)
    OA-->>APP: Action::LoginSucceeded
    APP->>APP: notify "signed in"; Goto(Home); FetchMe
    APP->>API: me()  (GET /api/v1/me)
    API-->>APP: Action::SetUser(email)
    Note over APP: header now shows "email @ identity-host"
```

On failure the flow dispatches `Action::LoginFailed`, which resets the Login
screen and shows an error notification.

## CLI commands

The same building blocks back the non-TUI subcommands (`src/main.rs` →
`src/auth/mod.rs`):

- `engineer login` — runs the browser flow, stores the refresh token, prints the
  user.
- `engineer logout` — revokes the refresh token server-side (best-effort) and
  deletes the keyring entry.
- `engineer whoami` — refreshes an access token and prints the current user.

## Security notes

- **PKCE** protects the public client — no client secret is shipped.
- **Loopback redirect** keeps the authorization code on the local machine.
- **Keyring** storage avoids plaintext token files; access tokens are
  memory-only.
- The Bearer token is **never logged** (see [api-layer.md](./api-layer.md) →
  redaction).

[`keyring`]: https://docs.rs/keyring
