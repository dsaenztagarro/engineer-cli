# engineer-cli

Terminal client for [Engineer](../engineer), the study-tracking app. Authenticates against [Identity](../identity) over OAuth2 (Authorization Code + PKCE, RFC 8252) and lets you log activities and update reading progress without leaving the terminal.

Keybindings are minimalistic and follow neovim muscle memory: `j`/`k`, `gg`/`G`, `/`, `n`/`N`, `:cmd`, `<Space>` as leader, `i`/`Esc` for insert/normal in forms.

## Setup

There is none — install the binary and run it:

```sh
cargo run            # launches the TUI against production (identity.dsaenz.dev)
```

The production URLs are baked-in defaults, so a bare `cargo run` (or the
installed release binary) targets production with no configuration. There is **no
separate dev build** — the target environment is chosen at **runtime** via env
vars or a config file, not at compile time (see [Choosing the environment](#choosing-the-environment)
below). The same binary works for both.

On first launch you'll land on a **Sign in** screen. Press `Enter`, complete the
login in your browser, and you're dropped into the authenticated TUI. The
refresh token is stored in your OS keyring; subsequent launches go straight in.

No config file, no client registration. The client identity is an OAuth **Client
ID Metadata Document** (CIMD): the Engineer app serves a small JSON document at
`/.well-known/oauth-client/engineer-cli.json`, and Identity fetches it to resolve
the client. The TUI's `client_id` is that document's URL, derived from the API
host.

## Commands

```
engineer            # launch TUI (default); prompts for login if needed
engineer login      # run the OAuth flow from the shell, store refresh token in OS keyring
engineer logout     # revoke + delete keyring entry
engineer whoami     # print the authenticated user

engineer timer                     # one-line read of the live timer (--json for the full read)
engineer timer status [--short]    # fixed-order status string; --short = glyph + clock for status bars
engineer timer start [query]       # start — fuzzy-binds to an activity, or unnamed when bare
engineer timer start <q> --switch  # stop & save the running timer, then start
engineer timer toggle              # pause ⇄ resume (bind it in tmux/zellij)
engineer timer pause|resume|stop   # stop refuses on an unbound timer — bind or discard first
engineer timer bind <query>        # name a running unnamed timer
engineer timer discard [--force]   # throw the timer away; past ~2 minutes requires --force
engineer timer settings [--json]   # the per-user timer knobs, read-only (edit on the web)
```

Timer exit codes answer "is the clock counting?": `0` counting (running / focus
work) · `1` nothing running (and verb refusals) · `3` idle, reclaim pending ·
`4` not counting (paused / focus break). Output is plain when piped — ANSI
colour only on a TTY, and never when `NO_COLOR` is set.

## How authentication works

engineer-cli is a **native loopback client** (RFC 8252). The `redirect_uris` in
the metadata document describe where the OAuth callback lands, and that's always
the **user's own machine** — never the server. This is why the document always
returns loopback URLs (`http://127.0.0.1/callback`, `http://localhost/callback`)
in **every** environment, including production. The flow:

1. The TUI binds an ephemeral port on `127.0.0.1` (`TcpListener::bind("127.0.0.1:0")`
   in `src/auth/oauth.rs`).
2. The browser opens Identity's authorize page (on `identity.dsaenz.dev` in prod).
3. After consent, Identity redirects the **browser** back to
   `http://127.0.0.1:<port>/callback` — which works because the browser is on the
   same machine as the TUI.
4. The TUI's local listener catches the authorization code and exchanges it.

So the redirect target is the loopback interface on your laptop, not
`engineer.dsaenz.dev`. It stays `127.0.0.1`/`localhost` regardless of whether
you're pointing at localhost or prod servers. What *does* change between dev and
prod is the **`client_id`** (the document's own URL — `https://engineer.dsaenz.dev/...`
vs `http://localhost:4001/...`) and the host serving the document; the
`redirect_uris` are environment-independent on purpose.

Two details worth noting:

- The bare `http://127.0.0.1/callback` (no port) relies on Identity's RFC 8252
  loopback **port-stripping** (`doorkeeper_loopback_patch.rb`) to match the
  runtime `:<port>`. This is the same mechanism the old manual registration used.
- RFC 8252 explicitly **allows `http://` for loopback** even in production — it's
  exempt from the HTTPS-redirect requirement (Identity's `force_ssl_in_redirect_uri`
  whitelists loopback hosts). So `http://` here is not a downgrade; it's the
  spec-correct choice for a native client.

## Choosing the environment

There is **no separate dev/prod build** — the same binary targets either, chosen
at **runtime**. Config is read on every launch, so switching environments or
editing `config.toml` never requires recompiling. The `client_id` (CIMD document
URL) is derived from `api_url`, so it follows whichever host you pick.

The simplest switch is the `--env` flag (or the `ENGINEER_ENV` env var), which
selects a built-in preset — no config file needed:

```sh
cargo run                        # production (default): *.dsaenz.dev
cargo run -- --env development   # localhost: identity :4000, engineer :4001
ENGINEER_ENV=development cargo run

cargo run --release              # optimized build, production servers
```

Accepted values: `production` (alias `prod`) and `development` (aliases `dev`,
`local`). Note the `--` in `cargo run -- --env …`: it passes the flag to the app
rather than to Cargo. The installed binary takes it directly: `engineer --env development`.

### Custom URLs and overrides

For anything other than the two presets, override individual values via
`config.toml` or `ENGINEER_*` env vars. The config file lives at
`~/.config/engineer-cli/config.toml` (honoring `XDG_CONFIG_HOME`) on **all**
platforms, including macOS:

```toml
identity_url = "http://localhost:4000"
api_url      = "http://localhost:4001"
# client_id  = "..."   # optional; derived from api_url when omitted
# scopes     = "read write"
```

Layering, later wins: **environment preset (`--env`/`ENGINEER_ENV`) < `config.toml` <
`ENGINEER_*` env vars**. Each layer overrides only the keys it sets. So you might
keep custom URLs in `config.toml` for everyday work and still redirect a single
run with `ENGINEER_IDENTITY_URL=… ENGINEER_API_URL=… engineer`.

Per-key env vars: `ENGINEER_IDENTITY_URL`, `ENGINEER_API_URL`, `ENGINEER_CLIENT_ID`,
`ENGINEER_SCOPES`.

In development Identity must be allowed to fetch the HTTP (non-HTTPS) metadata
document — it sets `config.x.cimd_allow_http = true` in its development
environment for exactly this.
