# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Timer screen + persistent header cell.** A new `Timer` screen owns the daily-loop timekeeping moments: start a blank clock ("name it later"), bind a running timer to work by searching `/api/v1/timer/candidates` or minting a new activity from a typed title, pause/resume, and stop — which shows the written segment (minutes → activity) so the ledger is trusted. On top, a fixed-width **header cell on every screen** renders the live timer per the web pill contract: an accent `● mm:ss` (widening to `h:mm:ss`) when running, an amber `‖ mm:ss` when paused, and nothing when absent — never the activity title. The snapshot lives in app state and is polled every 15s from `GET /api/v1/timer`, with the displayed elapsed ticked locally between polls. Reachable from Home with `t` (or the `<Space>` leader `t`, or `:timer`); on-screen keys are `s` start · `/` bind · `p` pause/resume · `x` stop · `d` discard.

- **Progress screen (`engineer pace` meters).** A new read-only screen renders the weekly pace aggregate from `GET /api/v1/progress` as one ASCII meter per target, behind-first: a block-bar with a now-tick (where the week expects you to be), `actual/target` hours, the signed delta, and the pace state (`behind` amber / `on pace` green / `met`). A week-header line (`2026-W27 · sat · day N of 7 · now = NN%`), a behind-total footer naming the largest gap, and a compact kind-mix line round it out. Reachable from Home with `p` (or the `<Space>` leader `3`/`p`); step weeks with `[` / `]` and return to the current week with `t`. Pace is derived from segments at read time, so nothing is stored client-side.

## [0.2.0] - 2026-06-21

### Changed

- **Renamed the project from `engineer-tui` to `engineer-cli`.** The package name, OS keyring service, XDG config/state directories (`~/.config/engineer-cli/`, `~/.local/state/engineer-cli/`), log filename prefix, tracing namespace (`engineer_cli::api`), log env var (`ENGINEER_CLI_LOG`), and the CIMD `client_id` document path (`engineer-cli.json`) all moved to the new name. The installed binary remains `engineer`. **Upgrade note:** because the keyring service name changed, existing installs must re-run `engineer login`; any `~/.config/engineer-tui/config.toml` should be moved to `~/.config/engineer-cli/config.toml`.

### Added

- **Prebuilt binaries and Homebrew distribution via [cargo-dist](https://opensource.axo.dev/cargo-dist/).** Tagged releases now cross-build macOS (arm64/x86_64) and Linux (arm64/x86_64) binaries, publish them to GitHub Releases with a shell installer, and push an `engineer` formula to the [`dsaenztagarro/homebrew-tap`](https://github.com/dsaenztagarro/homebrew-tap) tap. Install with `brew install dsaenztagarro/tap/engineer`.

## [0.1.0] - 2026-06-19

### Added

- Initial release: a neovim-flavored terminal client for [Engineer](https://github.com/dsaenztagarro/engineer) to log activities and track reading progress, authenticating against [Identity](https://github.com/dsaenztagarro/identity) over OAuth2 Authorization Code + PKCE (RFC 8252).
- **Zero-friction authentication via an OAuth Client ID Metadata Document (CIMD).** The client identity is the URL of a metadata document served by the Engineer app; Identity fetches it server-side. The `client_id` is derived from `api_url`, so no manual client registration or pasted credentials are needed.
- **In-TUI sign-in screen.** Launching without a stored refresh token lands on a "Sign in" screen; pressing Enter runs the browser OAuth flow and drops into the authenticated TUI. The refresh token is stored in the OS keyring; subsequent launches go straight in. The `login`, `logout`, and `whoami` subcommands remain for scripting.
- **Explicit environment selection** via the `--env` flag and `ENGINEER_ENV` variable (`production` default, or `development` for localhost), with built-in URL presets so a fresh run needs no config file. Layered configuration: environment preset < `~/.config/engineer-cli/config.toml` (XDG-honored on all platforms, including macOS) < `ENGINEER_*` env vars.
- **GitHub Actions CI** running `cargo test` on pushes to `master` and on pull requests.

[Unreleased]: https://github.com/dsaenztagarro/engineer-cli/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/dsaenztagarro/engineer-cli/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dsaenztagarro/engineer-cli/releases/tag/v0.1.0
