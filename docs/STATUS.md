# Project Status

<!--
  Verify test counts: cargo test --workspace --lib 2>&1 | awk '/^test result:/'
  Last verified: July 2026 — 181 unit + 7 integration = 188 total, 0 failures.
-->

**Last updated:** July 2026

## Build & test

```
cargo build     → 0 production warnings
cargo test      → 188 tests passed, 0 failed (181 unit + 7 integration)
cargo clippy    → 0 correctness warnings (~57 style warnings, all cosmetic)
cargo audit     → 0 vulnerabilities, 1 warning (async-std unmaintained)
```

| Crate | Tests | Status |
|---|---|---|
| `app` | 71 | Stable |
| `cache` | 15 | Stable |
| `config` | 24 | Stable |
| `core` | 10 | Stable |
| `mail-store` | 30 | Stable |
| `pgp` | 11 | Stable |
| `search` | 8 | Stable |
| `smtp-client` | 6 | Stable |
| `ui` | 6 | Stable |
| `integration` (mail-store) | 7 | Stable |

**Total:** 188 tests.

## Maturity by subsystem

| Subsystem | Maturity | Notes |
|---|---|---|
| IMAP IDLE | Production | RFC 2177, backoff reconnect, UIDVALIDITY, STARTTLS, OAuth2 refresh, switchable |
| SMTP send | Production | TLS/STARTTLS, configurable auth, MIME multipart, PGP sign+encrypt |
| Local cache | Production | Per-account, write-through, configurable caps with eviction, body caching schema |
| Message threading | Production | JWZ algorithm, sorted, tested |
| Multi-account | Production | Per-account tasks, state isolation, 4 isolation tests |
| Full-text search | Production | Incremental, configurable max 50k, fair cross-account eviction |
| TUI rendering | Production | ratatui, detail/compose/search/passphrase modes, sidebar with create/delete |
| Compose + reply | Production | Reply/reply-all, draft save, PGP sign/encrypt, in-TUI passphrase prompt |
| Message detail | Production | Headers, scrollable body, attachments, PGP status, HTML open (sanitized) |
| Flag manipulation | Production | Optimistic update + async sync + rollback via refetch |
| Expunge | Production | Manual trigger (`$`), immediate refetch |
| Copy/move | Production | UID COPY, COPY+STORE+EXPUNGE fallback, integration tested |
| Mailbox browser | Production | Live LIST, SPECIAL-USE labels, create/delete |
| PGP sign | Production | Per-account key, in-TUI passphrase, `Zeroizing<String>` cache |
| PGP encrypt | Production | Keyring directory, recipient lookup, never-downgrade |
| Body fetch | Production | On-demand, RFC822.SIZE check (25MB default), parse text+HTML |
| HTML open | Production | `H` key, ammonia-sanitized, OS browser |
| Attachment save | Production | Collision handling, path-traversal protection |
| CLI | Production | clap-based, env var layering, --account validation |
| OS notifications | Production | Configurable, graceful failure |
| Error surfacing | Production | Event::Error → UI status bar, structured logging |
| Channel backpressure | Production | Bounded 256/64/2/1, per-channel policy, rate-limited log |
| Incoming size limit | Production | RFC822.SIZE check, configurable 25MB |
| Storage limits | Production | Caps for cache (10k), search (50k), contacts (5k) with fair eviction |
| Structured logging | Production | `log`+`env_logger`, `RUST_LOG` filterable, zero `eprintln!` |
| Cache retry | Production | 3 attempts 100→300ms, surfaced to UI |
| Flag rollback | Production | Server failure → refetch canonical state |
| OAuth2 token refresh | Production | RFC 6749, `reqwest`-based, redacted creds, mock tested |
| In-TUI passphrase | Production | Masked input, cache miss → prompt → cache, cancel restores |
| Integration tests | Production | 7 tests against Greenmail (IMAP), covering fetch/flags/copy/move/append/IDLE |

## Current limitations

- **MOVE uses fallback.** async-imap 0.11 lacks MOVE. COPY+STORE+EXPUNGE fallback.
- **No attachment caching.** Body text is cached and read from cache on startup; attachment bytes are not yet persisted.
- **No rich HTML in TUI.** HTML converted to plaintext; external browser for original.
- **async-std transitive dep.** From async-imap. `mxr-async-imap` 0.10 has `runtime-tokio` but is API-incompatible.
- **Mailbox rename not implemented.** Create/delete supported; rename is not.

## Platform

- **macOS:** Primary target. Full build and test.
- **Linux:** Expected to work.
- **Windows:** Not tested.

## Dependencies

~310 transitive crates. Heaviest: `sequoia-openpgp`, `tantivy`, `ratatui`+`crossterm`, `rusqlite` (bundled), `tokio`. 0 vulnerabilities, 1 unmaintained warning.
