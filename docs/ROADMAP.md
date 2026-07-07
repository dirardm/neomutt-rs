# Roadmap

## Implemented

### Core infrastructure
- [x] Domain types: `Message`, `Envelope`, `Mailbox`, `FlagSet`, `Attachment`, `MailboxEntry`
- [x] MIME parsing, RFC 2047 decoding, body text with HTML→text fallback, HTML body extraction
- [x] Attachment metadata (filename, content-type, size)
- [x] JWZ threading (References/In-Reply-To)
- [x] Fixture `.eml` tests (plain, multipart, RFC 2047, with attachment, HTML-only)

### IMAP
- [x] TCP+TLS (Direct), STARTTLS upgrade, Plain (testing only)
- [x] LOGIN and XOAUTH2 authentication with automatic token refresh (OAuth2)
- [x] LIST mailboxes with SPECIAL-USE labels (📥📤🗑📝🚫📦)
- [x] Headers-only fetch (FLAGS + BODY.PEEK[HEADER])
- [x] On-demand body fetch (BODY.PEEK[]) with RFC822.SIZE size-limit, cached to SQLite
- [x] On-demand MIME part fetch
- [x] IMAP IDLE (RFC 2177) with poll fallback
- [x] Switchable IDLE (SELECT new mailbox on same connection)
- [x] Exponential backoff reconnect
- [x] UIDVALIDITY tracking + cache invalidation
- [x] Flag manipulation: STORE +FLAGS/-FLAGS (optimistic + async sync + rollback)
- [x] EXPUNGE: manual trigger (`$`), immediate refetch
- [x] COPY message (UID COPY)
- [x] MOVE message (COPY + STORE \Deleted + EXPUNGE fallback)
- [x] APPEND with flags (draft save)
- [x] 9 Greenmail integration tests (connect/fetch/flags/copy/move/append/IDLE/OAuth2 mock)
- [x] Error surfacing: all failures → `ImapEvent::Error` → UI status bar

### SMTP
- [x] Send via `lettre`, TLS/STARTTLS (configurable per-account), MIME multipart
- [x] In-Reply-To / References headers
- [x] `builder_dangerous` removed, regression test enforced

### Local storage
- [x] SQLite cache (messages by account+mailbox+uid)
- [x] Body text caching (read from cache before fetch, write after)
- [x] Per-mailbox UIDVALIDITY, flag persistence, cold-start
- [x] Contacts table with recency-based eviction
- [x] Configurable limits: 10k messages/mailbox, 50k search, 5k contacts

### Search
- [x] Full-text tantivy index (subject, from, body-if-fetched)
- [x] Incremental indexing, body re-index on fetch
- [x] Account-scoped and field-scoped queries
- [x] Configurable max 50k with fair cross-account eviction

### PGP
- [x] Encrypt/decrypt/sign/verify via sequoia-openpgp
- [x] Per-account key config, keyring for recipient lookup
- [x] Encrypt in send path (never silently downgrades)
- [x] In-TUI passphrase prompt (masked input, cancel, retry on wrong)
- [x] Passphrase cache with `Zeroizing<String>` (zeroed on drop)
- [x] `sign_unlocked` wired into compose send path

### UI
- [x] Message list pane with unseen indicator
- [x] Message detail view (headers, scrollable body, attachments, PGP status)
- [x] Status bar with error display, new-mail badge, key hints
- [x] Threaded display (indentation, toggle `t`)
- [x] Compose view (To/Subject/Body, PGP toggles, attached files, autocomplete)
- [x] Reply/Reply-all with pre-fill
- [x] Search overlay (`/`)
- [x] File browser for attachments (`a`)
- [x] Mailbox sidebar with live LIST, SPECIAL-USE labels, create (`n`), delete (`D`), destination picker
- [x] Account switcher (`[`/`]`)
- [x] Vim-style navigation (`j`/`k`)
- [x] Open sanitized HTML in browser (`H`)
- [x] Passphrase prompt overlay (masked, Enter/Esc)

### Multi-account
- [x] TOML config with `[[accounts]]`, env-var fallback
- [x] One IMAP task per account, per-account state/cache/PGP/SMTP
- [x] Deterministic account ordering

### Production readiness
- [x] Error surfacing (persists across updates)
- [x] New-mail detection (UID diff, accumulator badge)
- [x] CLI (`--config`, `--account`, `--version`, `--help`)
- [x] Configurable OS notifications with preview
- [x] Structured logging (`log`+`env_logger`, `RUST_LOG` filterable, zero `eprintln!`)
- [x] Bounded channels (256/64/2/1) with per-channel backpressure policy
- [x] Configurable incoming message size limit (25MB default)
- [x] Cache write retry (3 attempts)
- [x] Optimistic flag rollback on server failure
- [x] Credential redaction in all Debug impls
- [x] HTML sanitization (ammonia) before browser open
- [x] Path traversal protection for attachment save
- [x] `.gitignore` with comprehensive exclusions
- [x] 200 tests (191 unit + 9 integration), 0 failures

## Open / planned

### Near-term
- [ ] IMAP MOVE via native MOVE command (when async-imap adds it)
- [ ] PGP/MIME (multipart/signed, multipart/encrypted)
- [ ] Attachment caching (bytes in SQLite)
- [ ] Replace async-imap with tokio-native library (eliminate async-std warning)

### Medium-term
- [ ] Mailbox rename
- [ ] Markdown-to-HTML for compose
- [ ] Unified inbox across accounts
- [ ] Color themes (TOML)
- [ ] Mouse support

### Long-term
- [ ] Offline outbox
- [ ] Notmuch-style virtual folders
- [ ] Scripting for UI extensions
- [ ] IMAP NOTIFY
- [ ] JMAP protocol
- [ ] Windows support
- [ ] Packaging (brew, .deb, .rpm)
