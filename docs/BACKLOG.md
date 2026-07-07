# Backlog

Unordered collection of concrete, actionable items beyond the roadmap.
Verified July 2026 — all items genuinely unimplemented.

## IMAP

- [ ] **Native MOVE command.** When async-imap adds MOVE (RFC 6851), switch from COPY+EXPUNGE fallback.
- [ ] **OAuth2 token auto-refresh.** Detect 401 on IMAP commands and trigger the existing `refresh_access_token` flow automatically.
- [ ] **IMAP NOTIFY.** Alternative to IDLE (RFC 5465).
- [ ] **Mailbox rename.** Rename an existing mailbox from the sidebar. Create/delete already supported.

## UI

- [ ] **Color themes.** Configurable TOML color scheme.
- [ ] **Mouse support.** crossterm mouse events.
- [ ] **Notification history.** Log of recent error/status messages.
- [ ] **Sidebar width configurability.** Currently hardcoded at 20 cols.
- [ ] **Sidebar refresh keybinding.** Dedicated key to re-LIST mailboxes.

## Security

- [ ] **Content-Disposition: inline vs attachment detection.** All non-text parts treated as attachments.
- [ ] **PGP/MIME.** `multipart/signed` and `multipart/encrypted` instead of inline PGP.

## Search

- [ ] **Search result highlighting.**
- [ ] **Date-range queries.**
- [ ] **Saved searches / virtual folders.**

## PGP

- [ ] **Key generation in app.**
- [ ] **WKD / keyserver lookup** for recipient public keys.
- [ ] **In-TUI passphrase for decrypt.** Sign flow is wired; decrypt operations still use env/stdin.

## Cache & persistence

- [ ] **Draft auto-save.** Periodically save compose state.
- [ ] **Offline outbox.** Queue sends, flush on reconnect.
- [ ] **Body and attachment caching.** Schema exists, not yet wired at the fetch site. Store fetched body text and attachment bytes.

## Multi-account

- [ ] **Unified inbox.** Merge all accounts by date.
- [ ] **Per-account default mailbox config.**
- [ ] **Quick-switch keybindings** for specific accounts.

## Operations

- [ ] **Resolve async-std warning.** Migrate to tokio-native IMAP client when a mature alternative ships.

## General

- [ ] **Integration tests in CI.** Run Greenmail-based tests automatically.
- [ ] **Benchmarks.** Threading on 100k msgs, search throughput.
- [ ] **Windows support.**
- [ ] **Packaging.** brew, .deb, .rpm, pre-built binaries.
