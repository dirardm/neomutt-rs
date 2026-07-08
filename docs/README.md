# neomutt-rs

**A terminal email client written in Rust** — inspired by [neomutt](https://neomutt.org/),
built from scratch with modern async I/O, local caching, full-text search,
and multi-account support.

---

## AI Experiment Disclosure

**This repository is an experiment in AI-agent-driven software development.**

Every line of code, every architectural decision, and every test in this codebase was produced by prompting an AI coding agent (**DeepSeek**) turn by turn. No code was written by hand. The prompts were authored by **Dirard Mikdad**.

The goal was to answer: _can a fully-AI-prompted workflow produce a real, functional, non-trivial desktop application from scratch?_ The result is this: a working terminal email client with IMAP IDLE, SMTP TLS, PGP sign/encrypt, full-text search, SQLite caching, and a ratatui-based terminal UI — ~7,000 lines of library code + ~350 lines of binary glue, built entirely through sequential prompting sessions.

### Status

This is a **beta-stage experiment**, not a production-ready mail client. See [STATUS.md](STATUS.md) for a detailed maturity assessment. **Do not use this as a daily driver for anything sensitive without independent review.** The codebase handles credentials, PGP keys, and network connections — the security-relevant surface area is real. While extensive hardening has been applied (TLS everywhere, credential redaction, zeroization, path-traversal protection, bounded channels, storage limits, structured logging, integration tests), this project has never been audited by a human security reviewer.

**Usage is at your own risk. No warranty is given, expressed or implied.**

### Contributors

- **Dirard Mikdad** — Prompts
- **DeepSeek** — Coding

---

## Quick start

```bash
git clone https://github.com/dirardm/neomutt-rs.git
cd neomutt-rs
cargo build --release

# Single-account setup (env vars)
export IMAP_HOST=imap.example.com
export IMAP_USER=me@example.com
export IMAP_PASS=your-password

cargo run -p neomutt
```

Or with a config file (`~/.config/neomutt-rs/config.toml`):

```toml
[notifications]
enabled = true
show_preview = true

[downloads]
directory = "~/Downloads"
max_attach_size = 26214400

[[accounts]]
name = "work"
imap_host = "imap.work.com"
imap_user = "me@work.com"
imap_pass = "s3cret"
smtp_server = "smtp.work.com"
smtp_port = 587
pgp_key_path = "~/.pgp/work-secret.asc"
pgp_keyring_dir = "~/.pgp/recipients/"

[[accounts]]
name = "personal"
imap_host = "imap.personal.com"
imap_user = "me@personal.com"
imap_pass = "personal123"
smtp_server = "smtp.personal.com"
```

## Features

- **Multi-account** — one IMAP IDLE task per account, per-account mailbox state
- **Real-time updates** — IMAP IDLE (RFC 2177) with automatic poll fallback
- **Mailbox management** — live folder list, SPECIAL-USE labels, sidebar browser, create/delete
- **Copy/move** — copy or move messages between mailboxes (`C`/`M` keys)
- **Threading** — Jamie Zawinski's algorithm, toggleable with `t`
- **Full-text search** — tantivy-backed, incremental indexing, field-scoped queries
- **PGP** — sign + encrypt outgoing mail; in-TUI passphrase prompt; keyring directory; zeroize-hardened session cache
- **Local cache** — SQLite persistence per account, configurable eviction limits
- **Compose** — compose, reply, reply-all, file attachments, autocomplete contacts, sign/encrypt toggles
- **Attachments** — MIME parsing, on-demand part fetch, MIME multipart sending, save to disk
- **Flag manipulation** — mark read/unread (`m`), star (`*`), delete (`d`), expunge (`$`)
- **Body fetch** — on-demand body download via `Enter`, HTML-to-plaintext fallback
- **Message detail view** — full-screen with headers, scrollable body, attachment list, PGP status
- **Open HTML in browser** — `H` key opens sanitized HTML in OS browser
- **STARTTLS** — plain TCP upgrade path (port 143 → TLS)
- **OAuth2/XOAUTH2** — SASL authentication with automatic token refresh
- **Desktop notifications** — configurable OS notifications with sender+subject preview
- **Error surfacing** — IMAP/auth/cache errors visible in UI status bar
- **Vim-style keys** — `j`/`k` navigation, `/` search, `c` compose, `r` reply
- **Structured logging** — `log`+`env_logger`, `RUST_LOG` filterable
- **Integration tests** — 7 tests against Greenmail IMAP server
- **200 tests** — 191 unit + 9 integration, 0 failures

## Project structure

| Crate | Role |
|---|---|
| `core` | Domain types: `Message`, `Envelope`, `Mailbox`, `FlagSet`, `Attachment`, threading, body parsing |
| `cache` | SQLite persistence: messages, bodies, attachments, UIDVALIDITY, contacts |
| `config` | TOML account loading, notification/download/display preferences, env-var fallback |
| `mail-store` | IMAP client: connect, IDLE, poll, fetch headers/parts/bodies, flags, copy/move, append, OAuth2 refresh |
| `smtp-client` | Send email via `lettre`, TLS/STARTTLS, MIME multipart attachments |
| `ui` | Terminal UI: ratatui + crossterm, message list, detail view, compose, search, file browser, passphrase prompt |
| `pgp` | PGP encrypt/decrypt/sign/verify via `sequoia-openpgp`, keyring, zeroize-hardened cache |
| `search` | Full-text search index via `tantivy` |
| `app` | Binary: tokio runtime, channel wiring, bounded backpressure, state machine |

## Documentation

- [User Manual](USER_MANUAL.md) — keybindings, configuration, features
- [Architecture & Tech Stack](ARCHITECTURE.md)
- [Project Status](STATUS.md) — maturity assessment
- [Backlog](BACKLOG.md) — unimplemented items
- [Security](SECURITY.md) — advisory tracking, hardening
- [Current State Assessment](CURRENT-STATE.md) — detailed per-feature analysis

## Known limitations

See [BACKLOG.md](BACKLOG.md) for the full list. Key items: async-std transitive dependency, no body/attachment caching, no rich HTML in TUI, mailbox rename not implemented.

## Integration tests

```bash
./ci/start-greenmail.sh
cargo test -p neomutt-mail-store --test integration_test -- --nocapture
```

## License

MIT OR Apache-2.0
