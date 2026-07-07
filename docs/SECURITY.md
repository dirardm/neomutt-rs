# Security

Last updated: July 2026

## Dependency advisory status

Advisories checked via `cargo audit`. Current: **0 vulnerabilities, 1 warning.**

| Advisory | Package | Status | Notes |
|---|---|---|---|
| RUSTSEC-2026-0204 | crossbeam-epoch 0.9.18 | ✅ Fixed | Updated to 0.9.20 via `cargo update` |
| RUSTSEC-2023-0071 | rsa 0.9.10 | ✅ Fixed | Upgraded to 0.10.0-rc.18 via direct dep in `pgp/Cargo.toml`. Ignored in `.cargo/audit.toml` |
| RUSTSEC-2025-0052 | async-std 1.13.2 | ⚠️ Monitored | Unmaintained (transitive via async-imap). No viable replacement exists |

## Already addressed

| Area | Status |
|---|---|
| Credential redaction (Debug) | ✅ `Account`, `ImapConfig`, `SmtpConfig` redact passwords, tokens, secrets |
| Path traversal | ✅ `sanitize_filename` + canonicalize containment check |
| PGP passphrase memory | ✅ `Zeroizing<String>` in cache, zeroed on drop, never logged or in RenderState |
| IMAP TLS | ✅ STARTTLS upgrade before auth, Direct uses TLS from connect |
| SMTP TLS | ✅ `starttls_relay()`/`relay()`, `builder_dangerous` only in `Plain` (testing), regression tested |
| HTML sanitization | ✅ `ammonia` strips scripts, iframes, remote images before browser open |
| Incoming size limit | ✅ RFC822.SIZE check before buffering, configurable 25MB |
| Channel backpressure | ✅ Bounded 256/64/2/1, per-channel policy, rate-limited log |
| Storage limits | ✅ Caps for cache (10k), search (50k), contacts (5k) with fair eviction |
| OAuth2 token refresh | ✅ RFC 6749 flow, `reqwest`-based, redacted, mock tested |
| Logging | ✅ `log`+`env_logger`, zero `eprintln!`, `RUST_LOG` filterable |
| .gitignore | ✅ target/, *.db, *.sqlite, .env, *.pem, *.key excluded |
