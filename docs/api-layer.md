# API layer

The API layer (`src/api/`) is the typed, async HTTP boundary to the Engineer
backend. It is intentionally small and follows conventions common to Rust
CLI/TUI clients: `reqwest` + `async`/`await`, `serde` models, a typed error
enum, an envelope for list responses, and `tracing` for observability.

## ApiClient

```rust
pub struct ApiClient { base: Url, http: reqwest::Client, auth: Auth }

enum Auth { Provider(TokenProvider), Static(String) }
```

- `base` is the API URL from `Config` (`http://localhost:4001` in dev,
  `https://engineer.dsaenz.dev` in prod).
- `auth` is either a `TokenProvider` (refreshes transparently — see
  [authentication](./authentication.md)) or a `Static` token. `with_token` is
  used by the CLI `whoami`/`login` paths and by tests.
- `ApiClient` is `Clone` (it wraps `reqwest::Client`, an `Arc` internally), so
  screens cheaply clone it into spawned tasks.

Constructors: `ApiClient::new(base, provider)` and
`ApiClient::with_token(base, token)`.

## Request pipeline

All verbs funnel through a small set of helpers in `src/api/mod.rs`:

```
request(method, path)   → attaches `Authorization: Bearer …` + `Accept: application/json`
get / post / patch      → build the typed request (query / JSON body)
send(req)               → execute, log, decode
```

`send` splits the builder with `RequestBuilder::build_split()` so it can log the
call on a dedicated tracing target before executing:

```rust
tracing::info!(target: "engineer_cli::api", %method, %url,
               status = status.as_u16(), latency_ms, "api call");
```

**Redaction:** the `Authorization` header / token is never logged — only method,
URL, status, and latency. URLs carry no secrets (query params like `status`/`q`
are safe). Response bodies are logged **only on error**, because success bodies
may contain PII (e.g. the user's email from `/api/v1/me`).

## Models & the list envelope

Resource models are plain `serde` structs (`Book`, `BookChapter`, `Activity`,
`Me`). Collection endpoints return a uniform envelope:

```rust
pub struct List<T> { pub data: Vec<T>, pub meta: Meta }
pub struct Meta { pub page: u32, pub per_page: u32, pub total: u32 }
```

Per-resource operations are added as `impl ApiClient` blocks in their own
modules, keeping transport generic and the domain calls local:

- `src/api/books.rs` — `list_books(status, q)`, `list_chapters`, `update_book`.
  Filters map to query params (`status=reading`, `q=…`).
- `src/api/activities.rs` — `list_activities`, `create_activity`.
- `src/api/timer.rs` — the single live timer: `timer()` (`GET /api/v1/timer` —
  a bare object, not the `List` envelope; carries `mode`/`phase`/
  `intervals_completed`/`idle`/`last_interacted_at`/`phase_started_at` and the
  overrun trio `planned_minutes`/`logged_minutes`/`over`, all serde-defaulted
  so older payloads decode), `timer_settings()` (`GET /api/v1/timer/settings`
  — the twelve read-only per-user knobs), `heartbeat_timer()`
  (`POST /api/v1/timer/heartbeat` — the presence beat that keeps the idle
  guard from tripping on real in-TUI work), `start_timer(activity_id, switch)`
  (`switch` stops & saves the running timer first),
  `pause_timer`/`resume_timer`/`stop_timer` (member `POST`s; stop refuses on
  an unbound timer), `bind_timer`, `timer_candidates(q)` (bare `Vec`),
  `discard_timer` (`DELETE /api/v1/timer`).
- `src/api/audit.rs` — the segment audit: `progress_audit()`
  (`GET /api/v1/progress/audit` — flagged rows + `audit_count`, flags derived
  on read) and `acknowledge_audit_segment(id)`
  (`PATCH /api/v1/progress/audit/segments/:id/acknowledge` — clears the
  duration-shape flags permanently; missing-metadata flags survive until
  fixed).
- `src/api/segments.rs` — completed segments, nested under their activity:
  `update_segment(activity_id, id, {minutes})`
  (`PATCH /api/v1/activities/:activity_id/segments/:id`, the trim preset) and
  `delete_segment(activity_id, id)` (the post-save undo / audit delete).
- `me()` (`src/api/mod.rs`) — `GET /api/v1/me`, the canonical current-user
  endpoint shared with the identity server and the MCP tools.

## Current-user endpoint convention

`me()` targets a **top-level singleton**, `GET /api/v1/me`, representing the
authenticated principal — resolved from the Bearer token, not from a path id.
This is the idiomatic REST shape for "the current user" (cf. GitHub `GET /user`,
Spotify `GET /me`).

It is deliberately **not** placed under the `users` collection
(`/api/v1/users/:id`): there, a router could match `me` as an `:id`, and the URL
would imply "me is a user id". Keeping `me` at the top level removes that
ambiguity and lets a future `/api/v1/users/:id` admin endpoint coexist cleanly.
Both the engineer API and the identity server serve the same `/api/v1/me` path;
on the engineer side it is backed by a dedicated `Api::V1::MeController#show`,
fully separate from any `/users/:id` resource.

## Error model

`ApiError` (`src/api/error.rs`) is a typed enum deriving `thiserror::Error`:

| Variant | Meaning |
|---------|---------|
| `Transport(String)` | network / reqwest failure before a response |
| `Decode(String)` | response body didn't match the expected type |
| `Unauthorized` | HTTP 401 |
| `Problem { status, title, detail, type_uri, errors }` | RFC 7807 `application/problem+json` |

`from_response` parses RFC 7807 problem documents (including the Engineer-specific
per-field validation `errors[]` extension on 422s) and degrades gracefully on
non-JSON bodies. `field_errors()` exposes the validation errors to form screens.
Domain code matches on `ApiError`; the binary boundary (`main.rs`) uses
`color_eyre` for human-readable reports.

## Alignment with Rust-ecosystem conventions

- **Async HTTP via `reqwest` (rustls).** rustls avoids a system OpenSSL
  dependency — standard for portable CLIs. `gzip`/`json` features enabled.
- **Typed error enum with [`thiserror`].** `ApiError` derives `thiserror::Error`
  with `#[error(...)]` messages — the idiomatic split: library code returns a
  typed error, and the binary boundary (`main.rs`) renders it with
  `color_eyre`/`eyre`.
- **`serde` everywhere** for request/response (de)serialization.
- **`tracing`** for structured, filterable observability rather than `println!`.
- **Transport/domain separation.** Generic `get/post/patch/send` vs.
  per-resource methods keeps the client easy to extend.

## Observability

Every call is recorded to the rolling log under the XDG state dir
(`~/.local/state/engineer-cli/`). Discover and tail it:

```bash
engineer --log-path                                   # prints the log directory
ENGINEER_CLI_LOG=engineer_cli::api=debug engineer tui # verbose API tracing
```

Inside the TUI, the `:logs` command surfaces the same path as a notification.

## Testing

The client is tested against a real local HTTP server with [`wiremock`] (see the
`#[cfg(test)]` modules in `src/api/mod.rs` and `src/api/books.rs`). This
exercises the actual URL paths and query params — for example, asserting `me()`
hits `/api/v1/me` and that `list_books(Reading, _)` sends `status=reading`.

[`thiserror`]: https://docs.rs/thiserror
[`wiremock`]: https://docs.rs/wiremock
