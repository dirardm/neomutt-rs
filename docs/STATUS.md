# Project Status

<!--
  Verify: cargo test --workspace 2>&1 | awk '/^test result:.*passed/'
  Last verified: July 2026
-->

**Last updated:** July 2026

## Build & test

```
cargo build     → 0 production warnings
cargo test      → 200 tests passed, 0 failed (191 unit + 9 integration)
cargo clippy    → 57 style warnings, 0 correctness warnings (all cosmetic)
cargo audit     → 0 vulnerabilities, 1 warning (async-std unmaintained)
```

| Crate | Tests |
|---|---|
| `app` | 71 |
| `cache` | 15 |
| `config` | 24 |
| `core` | 20 (10 unit + 10 fixture) |
| `mail-store` | 30 |
| `pgp` | 11 |
| `search` | 8 |
| `smtp-client` | 6 |
| `ui` | 6 |
| `integration` (mail-store) | 9 |
| **Total** | **200** |

## Maturity by subsystem

| Subsystem | Maturity | Notes |
|---|---|---|
| IMAP IDLE | Production | RFC 2177, backoff reconnect, UIDVALIDITY, STARTTLS, OAuth2 refresh, switchable |
| SMTP send | Production | TLS/STARTTLS, configurable auth, MIME multipart, PGP sign+encrypt |
| Local cache | Production | Per-account, write-through, configurable caps with eviction, body caching wired |
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
| Body fetch | Production | On-demand, RFC822.SIZE check (25MB default), parse text+HTML, cached |
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
| OAuth2 token refresh | Production | RFC 6749, `reqwest`-based, auto-refresh on auth failure, mock tested |
| Body caching | Production | Read from cache before fetch, write after, survives restart |
| In-TUI passphrase | Production | Masked input, cache miss → prompt → cache, cancel restores |
| Integration tests | Production | 9 tests against Greenmail (IMAP), covering fetch/flags/copy/move/append/IDLE/oauth2 |

## Current limitations

- **MOVE uses fallback.** async-imap 0.11 lacks MOVE. COPY+STORE+EXPUNGE fallback.
- **No attachment caching.** Body text is cached; attachment bytes are not yet persisted.
- **No rich HTML in TUI.** HTML converted to plaintext; external browser for original.
- **async-std transitive dep.** From async-imap. `mxr-async-imap` 0.10 has `runtime-tokio` but is API-incompatible.
- **Mailbox rename not implemented.** Create/delete supported; rename is not.

## Platform

- **macOS:** Primary target. Full build and test.
- **Linux:** Expected to work.
- **Windows:** Not tested.

## Dependencies

~310 transitive crates. Heaviest: `sequoia-openpgp`, `tantivy`, `ratatui`+`crossterm`, `rusqlite` (bundled), `tokio`. 0 vulnerabilities, 1 unmaintained warning.
