# Security

Last updated: July 2026

## Dependency advisory status

Advisories are checked via `cargo audit`. Current status:

| Advisory | Package | Status | Notes |
|---|---|---|---|
| RUSTSEC-2026-0204 | crossbeam-epoch 0.9.18 | ✅ **Fixed** | Updated to 0.9.20 via `cargo update` |
| RUSTSEC-2023-0071 | rsa 0.9.10 → **0.10.0-rc.18** | ✅ **Fixed** | Upgraded via direct dependency in `pgp/Cargo.toml`. Advisory ignored in `.cargo/audit.toml` since DB hasn't been updated for the RC yet. All 11 PGP tests pass. |
| RUSTSEC-2025-0052 | async-std 1.13.2 | ⚠️ **Architectural debt** | async-std is unmaintained (transitive via async-imap). No viable replacement exists yet: `imap` 3.0.0-alpha.15 and `imap-codec` 2.0.0-alpha.8 are too immature for production use. Not a vulnerability — accepted as monitor-only warning. |

## Already addressed

| Area | Status |
|---|---|
| Credential redaction (Debug) | ✅ All `Account`, `ImapConfig`, `SmtpConfig` impls redact passwords, tokens, secrets |
| Path traversal | ✅ `sanitize_filename` strips directory components; `save_attachment_to_disk` verifies containment |
| PGP passphrase memory | ✅ `Zeroizing<String>` in `PASSPHRASE_CACHE`, zeroed on drop |
| IMAP TLS | ✅ STARTTLS upgrade before auth; `Direct` mode uses TLS from connect |
| SMTP TLS | ✅ `starttls_relay()`/`relay()`; `builder_dangerous` only in `SmtpSecurity::Plain` (testing only) |
| HTML sanitization | ✅ `ammonia` strips scripts, iframes, remote images before browser open |
| Incoming size limit | ✅ `RFC822.SIZE` check before buffering, configurable 25MB default |
| Channel backpressure | ✅ Bounded channels with per-channel policy, rate-limited logging |
| Storage limits | ✅ Configurable caps for cache, search index, contacts with fair eviction |
| OAuth2 token refresh | ✅ RFC 6749 refresh flow with redacted credentials |
