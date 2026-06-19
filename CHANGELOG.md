# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-19

### Added

- Initial release: a neovim-flavored terminal client for [Engineer](https://github.com/dsaenztagarro/engineer) to log activities and track reading progress, authenticating against [Identity](https://github.com/dsaenztagarro/identity) over OAuth2 Authorization Code + PKCE (RFC 8252).
- **Zero-friction authentication via an OAuth Client ID Metadata Document (CIMD).** The client identity is the URL of a metadata document served by the Engineer app; Identity fetches it server-side. The `client_id` is derived from `api_url`, so no manual client registration or pasted credentials are needed.
- **In-TUI sign-in screen.** Launching without a stored refresh token lands on a "Sign in" screen; pressing Enter runs the browser OAuth flow and drops into the authenticated TUI. The refresh token is stored in the OS keyring; subsequent launches go straight in. The `login`, `logout`, and `whoami` subcommands remain for scripting.
- **Explicit environment selection** via the `--env` flag and `ENGINEER_ENV` variable (`production` default, or `development` for localhost), with built-in URL presets so a fresh run needs no config file. Layered configuration: environment preset < `~/.config/engineer-tui/config.toml` (XDG-honored on all platforms, including macOS) < `ENGINEER_*` env vars.
- **GitHub Actions CI** running `cargo test` on pushes to `master` and on pull requests.

[Unreleased]: https://github.com/dsaenztagarro/engineer-tui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/dsaenztagarro/engineer-tui/releases/tag/v0.1.0
