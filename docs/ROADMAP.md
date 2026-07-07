# Roadmap

## Implemented

### Core infrastructure
- [x] Domain types: `Message`, `Envelope`, `Mailbox`, `FlagSet`, `Attachment`, `MailboxEntry`
- [x] MIME parsing, RFC 2047 decoding, body text with HTML→text fallback, HTML body extraction
- [x] Attachment metadata (filename, content-type, size)
- [x] JWZ threading (References/In-Reply-To)
- [x] Fixture `.eml` tests (plain, multipart, RFC 2047, with attachment, HTML-only)

### IMAP
- [x] TCP+TLS (Direct) and STARTTLS upgrade
- [x] LOGIN and XOAUTH2 authentication
- [x] LIST mailboxes with SPECIAL-USE labels (📥📤🗑📝🚫📦)
- [x] Headers-only fetch (FLAGS + BODY.PEEK[HEADER])
- [x] On-demand body fetch (BODY.PEEK[]), HTML→text conversion
- [x] On-demand MIME part fetch
- [x] IMAP IDLE (RFC 2177) with 29-minute renewal, poll fallback
- [x] Switchable IDLE (SELECT new mailbox on same connection)
- [x] Exponential backoff reconnect
- [x] UIDVALIDITY tracking + cache invalidation
- [x] Flag manipulation: STORE +FLAGS/-FLAGS (optimistic + async)
- [x] EXPUNGE: manual trigger (`$`), immediate refetch
- [x] COPY message (UID COPY)
- [x] MOVE message (COPY + STORE \Deleted + EXPUNGE fallback, async-imap has no MOVE)
- [x] Error surfacing: all failures → `ImapEvent::Error` → UI status

### SMTP
- [x] Send via `lettre`, configurable auth, MIME multipart
- [x] In-Reply-To / References headers

### Local storage
- [x] SQLite cache (messages by account+mailbox+uid)
- [x] Per-mailbox UIDVALIDITY, flag persistence, cold-start
- [x] Contacts table with autocomplete

### Search
- [x] Full-text tantivy index (subject, from, body-if-fetched)
- [x] Incremental indexing, body re-index on fetch
- [x] Account-scoped and field-scoped queries

### PGP
- [x] Encrypt/decrypt/sign/verify via sequoia-openpgp
- [x] Per-account key config, keyring for recipient lookup
- [x] Encrypt in send path (never silently downgrades)
- [x] Passphrase handling: unlock encrypted keys, in-memory session cache

### UI
- [x] Message list pane with unseen indicator
- [x] Message detail view (headers, scrollable body, attachments, PGP status)
- [x] Status bar with error display, new-mail badge, key hints
- [x] Threaded display (indentation, toggle `t`)
- [x] Compose view (To/Subject/Body, PGP toggles, attached files, autocomplete)
- [x] Reply/Reply-all with pre-fill
- [x] Search overlay (`/`)
- [x] File browser for attachments (`a`)
- [x] Mailbox sidebar with live LIST, SPECIAL-USE labels, destination picker
- [x] Account switcher (`[`/`]`)
- [x] Vim-style navigation (`j`/`k`)
- [x] Open HTML in browser (`H`)

### Multi-account
- [x] TOML config with `[[accounts]]`, env-var fallback
- [x] One IMAP task per account, per-account state/cache/PGP/SMTP
- [x] Deterministic account ordering

### Production readiness
- [x] Error surfacing (persists across updates)
- [x] New-mail detection (UID diff, accumulator badge)
- [x] CLI (`--config`, `--account`, `--version`, `--help`)
- [x] Configurable OS notifications with preview
- [x] 118 tests, 1 pre-existing lint warning

## Open / planned

### Near-term
- [ ] IMAP MOVE via native MOVE command (when async-imap adds it)
- [ ] HTML sanitization before browser open (strip `<script>` tags)
- [ ] PGP/MIME (multipart/signed, multipart/encrypted)
- [ ] Passphrase prompt in TUI
- [ ] SMTP TLS (SMTPS/STARTTLS)
- [ ] OAuth2 token refresh

### Medium-term
- [ ] Mailbox create/delete/rename
- [ ] Draft saving
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
- [ ] vCard/LDAP import/export
- [ ] Windows support
- [ ] Packaging (brew, .deb, .rpm)
