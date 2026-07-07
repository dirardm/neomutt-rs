# neomutt-rs — Current State Assessment

**Date:** July 2026
**Overall Maturity:** Production-candidate

Verified against actual code (`HEAD`) — every claim checked directly against the source.

---

## 1. Feature Completeness vs. Daily-Driver Email Client

### Reading Mail

| Feature | Status | Evidence |
|---|---|---|
| Fetch (IMAP) | **Working** | `UID FETCH 1:* (FLAGS BODY.PEEK[HEADER])`, on-demand `fetch_body` with RFC822.SIZE size-limit, poll + IDLE loops functional. 7 integration tests against Greenmail |
| Threading | **Working** | JWZ algorithm, 5 tests, toggleable with `t`, indented rendering |
| HTML rendering | **Partial** | In-app: plaintext via `html2text`. Browser: sanitized via `ammonia`. No rich HTML in TUI |
| Attachments (view) | **Working** | `parse_attachments` walks MIME tree, detail view with filename/size/type, Tab cycles |
| Attachments (save) | **Working** | Collision-safe naming, path-traversal protection, configurable dir, tested |
| Search | **Working** | Tantivy full-text index, incremental on fetch+body load, configurable max 50k, 8 tests |

### Composing

| Feature | Status | Evidence |
|---|---|---|
| Plain compose | **Working** | `ComposeState` → `SendCompose` → `take_send_request` → `send_via_smtp` → TLS transport |
| Reply / Reply-all | **Working** | Pre-fills To, Subject (Re:), In-Reply-To, References. Reply-all includes all recipients |
| Attachments (outgoing) | **Working** | File browser → size-limit → MIME multipart/mixed via lettre |
| Drafts | **Working** | Save via `Ctrl+D` → `append_message_async` with `\Draft`. Edit existing drafts |
| PGP sign | **Working** | Per-account key, in-TUI passphrase prompt, `Zeroizing<String>` cache, never persisted |
| PGP encrypt | **Working** | Keyring directory, recipient lookup, never-downgrade, 2 tests |
| SMTP TLS | **Working** | `starttls_relay()` default or `relay()` SMTPS, configurable per-account |

### Mailbox Management

| Feature | Status | Evidence |
|---|---|---|
| Browse folders | **Working** | Sidebar from `LIST "" "*"`, SPECIAL-USE labels with heuristic fallback |
| Create mailbox | **Working** | `n` in sidebar → `create_mailbox_async` |
| Delete mailbox | **Working** | `D` in sidebar → confirmation → `delete_mailbox_async` |
| Copy message | **Working** | `C` → sidebar picker → UID COPY. Integration test passes |
| Move message | **Working** | `M` → COPY+STORE\Deleted+EXPUNGE fallback. Integration test passes |
| Flags | **Working** | Optimistic update + cache persist + async sync. Rollback on server failure via refetch |
| Expunge | **Working** | Server EXPUNGE + immediate header refetch |

### Multi-Account

| Feature | Status | Evidence |
|---|---|---|
| Account switching | **Working** | `[`/`]` keys, sorted list, per-account `AccountState`, 4 isolation tests |
| Per-account SMTP | **Working** | `smtp_config_for_account` from active account |
| Per-account IMAP | **Working** | One `idle_loop` task per account, per-account switch channel |

### Connectivity

| Feature | Status | Evidence |
|---|---|---|
| IMAP IDLE | **Working** | RFC 2177, auto poll fallback, re-enter IDLE after fetch |
| Reconnect/backoff | **Working** | Exponential 1s→30s cap, resets on success |
| STARTTLS | **Working** | Plain→TLS upgrade before auth, code reviewed |
| OAuth2/XOAUTH2 | **Working** | Token refresh via RFC 6749 flow, `reqwest`-based, redacted |
| SMTP TLS | **Working** | `builder_dangerous` removed, regression test enforced |

### In-TUI Passphrase Prompt

| Feature | Status | Evidence |
|---|---|---|
| Masked input | **Working** | Local buffer in event loop, never in RenderState |
| Cache miss → prompt | **Working** | `send_via_smtp` checks cache, shows prompt on miss |
| Submit → cache | **Working** | Passphrase cached via `sign_unlocked` for session |
| Cancel → restore | **Working** | `PassphraseCancel` restores compose with message intact |
| Wrong passphrase | **Working** | Error surfaced via `Event::Error`, prompt re-opens |

---

## 2. Reliability

### Failure Scenarios

| Scenario | Handling | Verdict |
|---|---|---|
| IMAP drops mid-IDLE | Inner break → outer reconnect with backoff | **Handled** |
| Auth fails at startup | `load_config().expect()` kills process. Runtime failures retry with backoff | **Partial** |
| Disk full during cache write | 3 retries 100→300ms, surfaced via Event::Error + status bar | **Handled** |
| Malformed email | `fetch_to_message` returns None, `mail_parser` handles nested MIME, HTML sanitized | **Handled** |
| Huge attachment | RFC822.SIZE check before buffering, 25MB default configurable | **Handled** |
| Network flakiness during send | Error → status bar, user stays in compose, message not lost | **Handled** |

### Resource Limits

| Resource | Status | Default |
|---|---|---|
| Channels | **Bounded** | 256/64/2/1 with per-channel policy, rate-limited logging |
| Incoming message size | **Capped** | 25 MB via RFC822.SIZE |
| Cache messages | **Capped** | 10 000/mailbox, lowest-UID eviction |
| Search index | **Capped** | 50 000 total, fair cross-account eviction |
| Contacts | **Capped** | 5 000, recency-based eviction |

---

## 3. Security

### Credential Redaction
All `Account`, `ImapConfig`, `SmtpConfig` Debug impls redact passwords, tokens, secrets. Tests verify. Zero `eprintln!` remaining — all structured logging via `log`+`env_logger`.

### PGP Passphrase
`PASSPHRASE_CACHE` uses `Zeroizing<String>`, zeroed on drop. Never serialized, never logged, never in RenderState. In-TUI prompt uses local event-loop buffer.

### TLS
- IMAP: `Direct` (TLS), `StartTls` (upgrade before auth), `Plain` (testing only)
- SMTP: `StartTls` default, `Tls` SMTPS, `Plain` (testing only). `builder_dangerous` only in `SmtpSecurity::Plain` arm — regression test enforces

### Path Traversal
`sanitize_filename` strips directory components. `save_attachment_to_disk` canonicalizes and verifies containment. New features use IMAP commands or `Path::join`.

### Dependency Audit
0 vulnerabilities. 1 warning: async-std 1.13.2 (unmaintained, transitive via async-imap). `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (rsa upgraded to 0.10.0-rc.18).

---

## 4. Test Suite

| Crate | Tests |
|---|---|
| `app` | 71 |
| `cache` | 15 |
| `config` | 24 |
| `core` | 10 |
| `mail-store` | 30 |
| `pgp` | 11 |
| `search` | 8 |
| `smtp-client` | 6 |
| `ui` | 6 |
| **Subtotal** | **181** |
| Integration (mail-store) | 7 |
| **Total** | **188** |

0 failures, 0 ignored. All pass reliably. No flaky patterns.

### High-Risk Path Coverage
- Cross-account isolation: 4 tests
- UIDVALIDITY handling: cache wipe + per-account
- Encrypt never downgrades: 2 tests
- Optimistic flag rollback: server sync failure → refetch
- SMTP TLS regression: `builder_dangerous_is_not_used` test
- OAuth2 refresh: mock HTTP test
- Passphrase prompt: wired via `sign_unlocked`

---

## 5. Code Health

- **Clippy:** 0 correctness warnings. ~57 style warnings (let_unit_value, cloned_ref_to_slice_refs, field_reassign_with_default) — all in test code or cosmetic
- **`Result<T, String>`:** zero remaining. All migrated to `thiserror` enums (`ConfigError`, `AppError`, `MailStoreError`)
- **TODO/FIXME:** zero in source code. All tracked in `docs/BACKLOG.md`
- **Logging:** `log`+`env_logger`, `RUST_LOG` filterable, millisecond timestamps, zero `eprintln!` remaining

---

## 6. Operational Readiness

### Documentation
- `README.md` — AI-experiment disclosure, features, quick start, crate map
- `USER_MANUAL.md` — keybindings, config schema, OAuth2, limits, integration tests
- `STATUS.md` — maturity table, limitations, platform support
- `BACKLOG.md` — 25 items, all verified against current code
- `SECURITY.md` — advisory tracking, hardening audit
- `mail-store/tests/README.md` — integration test setup
- `ci/start-greenmail.sh` — one-command test server startup

### Error Messages
All include operation context. Examples: `"flag sync failed (server rejected, locally removed): {e}"`, `"no public key found for: {addr}"`, `"message exceeds max fetch size ({size} > {max})"`.

### Disk
~2.5 GB (`target/debug/`). `.gitignore` prevents accumulation. `cargo clean` brings it to ~12 MB. Git repository: 42 files, 19,494 lines.

---

## Overall Rating: Production-candidate

All critical gaps closed: TLS everywhere, credential redaction, PGP zeroization, resource limits, structured logging, integration tests, passphrase prompt, OAuth2 refresh. Remaining scope: body/attachment caching, OAuth2 auto-refresh on 401, in-TUI passphrase for decrypt, async-std migration.
