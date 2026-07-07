# Architecture & Technical Stack

## Overview

neomutt-rs is a Cargo workspace of 9 crates (~7,000 lines of Rust) built
around a three-task async architecture.  The central design principle is
**single ownership of mutable state** — every piece of data has exactly one
writer, communicated via `tokio::sync::mpsc` bounded channels (256/64/2/1)
with per-channel backpressure policies.  No `Arc<Mutex<_>>` sprawl.

```
┌──────────────┐  ImapEvent  ┌──────────────┐  RenderState  ┌──────────────┐
│  IMAP tasks  │ ──────────→ │  App State   │ ────────────→ │   UI task    │
│(tokio::spawn)│             │   (main task)│               │ (spawn_block)│
│ 1 per account│             │  owns Mailbox│               │ ratatui TUI  │
└──────────────┘             └───────┬──────┘               └───────┬──────┘
          ↑                          │ Command                      │
     switch channel                  │←─────────────────────────────┘
     (mailbox redirection)           │
```

### Three long-lived tasks

1. **IMAP task** (`tokio::spawn`) — One per configured account.  Runs
   `idle_loop()` with a `tokio::select!` between IDLE notifications and
   mailbox-switch commands.  On switch, re-SELECTs the new mailbox on
   the same TCP+TLS connection.  Connects via Direct TLS or STARTTLS,
   authenticates via LOGIN or XOAUTH2.  Lists mailboxes (with
   SPECIAL-USE labels) on every (re)connect.  Falls back to 30 s polling
   if no `IDLE`.  Exponential backoff (1 s → 30 s).  All failures
   emit `ImapEvent::Error`.

2. **UI task** (`tokio::task::spawn_blocking`) — Owns the terminal.
   Renders ratatui TUI, polls keyboard every 50 ms, sends `Command`
   values.  Modes: message list, search, compose, message detail,
   file browser, mailbox sidebar (with destination picker for
   copy/move).

3. **App State task** (the `main` task) — Single owner of `Mailbox`.
   `tokio::select!` over event and command channels.  Mutates state,
   writes to cache + search, learns contacts, fires notifications,
   sends `RenderState` to UI.  Async operations (body fetch, flag
   sync, expunge, send, search, copy/move, attachment save) run
   via `tokio::spawn` / `spawn_blocking`.

### Crates

| Crate | Key deps | Key types / functions |
|---|---|---|
| **core** | `mail-parser`, `bitflags`, `rfc2047-decoder`, `html2text` | `Message`, `Envelope`, `Mailbox`, `FlagSet`, `Attachment`, `ThreadNode`, `thread_mailbox()`, `parse_attachments()`, `parse_body_text()`, `parse_html_body()` |
| **cache** | `rusqlite` (bundled) | `MailboxCache`, `Contact` — messages, uid_validity, contacts |
| **config** | `toml`, `serde` | `Account`, `ImapSecurity`, `NotificationConfig`, `DownloadConfig`, `load_config()` |
| **mail-store** | `async-imap`, `tokio`, `async-native-tls`, `tokio-util`, `reqwest` | `ImapClient`, `ImapConfig`, `MailboxEntry`, `FetchResult`, `idle_loop()`, `fetch_body()`, `fetch_part()`, `set_flags()`, `expunge()`, `copy_message()`, `move_message()`, `list_mailboxes()`, `ImapEvent`, `refresh_access_token()` |
| **smtp-client** | `lettre` | `OutgoingMessage`, `FileAttachment`, `send_message()` |
| **ui** | `ratatui`, `crossterm`, `tokio` | `RenderState`, `Command`, `Mode`, `ComposeState`, `FileEntry`, `selected_uid()` |
| **pgp** | `sequoia-openpgp` | `encrypt`, `decrypt`, `sign`, `verify`, `unlock_key`, `KeyStore`, `Keyring` |
| **search** | `tantivy` | `SearchIndex`, `SearchHandle`, `index_messages()`, `search()` |
| **app** | all above, `clap`, `notify-rust`, `open` | `AppState`, `AccountState`, `Args`, `apply_event/command`, `send_via_smtp`, `save_attachment_to_disk` |

### Data flow

```
Incoming mail:
  Server → IMAP task → ImapEvent::MailboxUpdated → App State
    ├─ Diff UIDs → new_mail_total accumulator
    ├─ Update per-account Mailbox
    ├─ UIDVALIDITY check → cache wipe if changed
    ├─ Write to cache (replace_mailbox)
    ├─ Index into search (index_messages)
    ├─ Learn contacts (learn_addresses)
    ├─ Fire OS notification (if genuinely_new > 0)
    └─ Push RenderState → UI redraws

Outgoing mail:
  UI keypress → Command::SendCompose → App State
    ├─ PGP sign (load cert, unlock key)
    ├─ PGP encrypt (keyring lookup)
    ├─ Read attached files from disk
    └─ send_via_smtp (MIME multipart) → lettre → SMTP

Copy/move:
  UI C/M → Command::CopyMessage/MoveMessage → sidebar opens
  → SelectMailbox(dest) → pending_copy_move_action set
  → tokio::spawn(copy/move_message_async)
    ├─ COPY uid to dest
    └─ (if move) STORE \Deleted + EXPUNGE on source
       → refetch dest if it's the active mailbox

Mailbox switch (live):
  UI b → sidebar → Enter → Command::SelectMailbox
  → switch channel → IMAP task: handle.done() → SELECT new_mb → idle().init()
  (same TCP+TLS connection, no reconnect)

Mailbox switch (sidebar):
  UI b → sidebar → j/k → Enter → SelectMailbox
  → App State: load cached messages for new mailbox
  → IMAP task: redirected via switch channel (live monitoring follows)
```

### Key design decisions

- **Flat storage, threaded view.** Threading is pure from `References`/`In-Reply-To`.
- **Headers-only fetch.** Bodies on demand. HTML→text via `html2text`, raw HTML retained for browser view.
- **Optimistic flag updates.** Local-first, async server sync.
- **Move via COPY+EXPUNGE.** async-imap 0.11 has no MOVE command. Fallback is functionally equivalent.
- **SPECIAL-USE labels.** Parsed from LIST response attributes + heuristic name matching.
- **Never silently downgrade.** Encrypt failures abort send. Flag sync errors surface to UI.

## Tech stack

| Layer | Technology |
|---|---|
| Language | Rust 1.96 (edition 2024) |
| Async | tokio (full) |
| IMAP | async-imap 0.11 via TLS, STARTTLS, or Plain (testing) |
| IMAP auth | LOGIN or XOAUTH2 with automatic token refresh |
| SMTP | lettre 0.11 via TLS/STARTTLS |
| TUI | ratatui 0.30 + crossterm 0.29 |
| DB | rusqlite 0.40 (bundled) |
| Search | tantivy 0.26 |
| PGP | sequoia-openpgp 2.4 (crypto-rust) |
| Config | toml 0.8 + serde |
| CLI | clap 4 |
| Notifications | notify-rust 4 |
| Browser open | open 5 |
| HTML→text | html2text 0.17 |
| Email parse | mail-parser 0.11, rfc2047-decoder 1.1 |

## Testing

200 tests across all crates (191 unit + 9 integration against Greenmail IMAP server). Unit tests use in-memory databases, generated PGP keys, temp directories, and fixture `.eml` files. Integration tests cover connect/list/fetch/flags/copy/move/append/IDLE/OAuth2-refresh against a real IMAP server. No live credentials required.
