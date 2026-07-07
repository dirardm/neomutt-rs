# User Manual

## Installation

```bash
cargo build --release -p neomutt
```

## Configuration

### Single account (env vars)

```bash
export IMAP_HOST=imap.example.com
export IMAP_PORT=993
export IMAP_USER=me@example.com
export IMAP_PASS=your-password

# Optional
export IMAP_SECURITY=starttls
export IMAP_OAUTH2_TOKEN=ya29...
export IMAP_OAUTH2_REFRESH_TOKEN=1//...
export IMAP_OAUTH2_CLIENT_ID=...
export IMAP_OAUTH2_CLIENT_SECRET=...
export IMAP_OAUTH2_TOKEN_ENDPOINT=https://oauth2.googleapis.com/token
export SMTP_SERVER=smtp.example.com
export SMTP_PORT=587
export SMTP_SECURITY=starttls
export SMTP_USER=me@example.com
export SMTP_PASS=smtp-pass
export PGP_SIGNING_KEY=~/.pgp/secret-key.asc
export PGP_KEYRING_DIR=~/.pgp/recipients/
export IMAP_MAILBOX=INBOX
export NEOMUTT_CACHE=~/.cache/neomutt-rs/neomutt.db
export NEOMUTT_SEARCH=~/.cache/neomutt-rs/search
export RUST_LOG=info
```

### Multiple accounts (TOML)

`~/.config/neomutt-rs/config.toml`:

```toml
[notifications]
enabled = true
show_preview = true

[downloads]
directory = "~/Downloads"
max_attach_size = 26214400

[imap_timeouts]
backoff_init_secs = 1
backoff_max_secs = 30
poll_interval_secs = 30
max_fetch_size_bytes = 26214400
max_cached_messages_per_mailbox = 10000
max_contacts = 5000

[search]
writer_buffer_bytes = 50000000
max_results = 50
max_indexed_messages = 50000

[display]
subject_width = 40
from_width = 30
date_width = 24
text_wrap_width = 80

[[accounts]]
name = "work"
imap_host = "imap.work.com"
imap_user = "me@work.com"
imap_pass = "s3cret"
smtp_server = "smtp.work.com"
smtp_port = 587
smtp_security = "starttls"
pgp_key_path = "~/.pgp/work-secret.asc"
pgp_keyring_dir = "~/.pgp/recipients/"

# OAuth2 (optional, takes precedence over password)
# imap_oauth2_token = "ya29..."
# imap_oauth2_refresh_token = "1//..."
# imap_oauth2_client_id = "..."
# imap_oauth2_client_secret = "..."
# imap_oauth2_token_endpoint = "https://oauth2.googleapis.com/token"
```

### CLI

```
neomutt --help
neomutt --version
neomutt --config /path/to/config.toml
neomutt --account work
```

## Keybindings

### Message list

| Key | Action |
|---|---|
| `↑/↓` or `j/k` | Navigate messages |
| `Enter` | Open message detail view |
| `q` | Quit |
| `c` | Compose new message |
| `r` | Reply |
| `R` or `a` | Reply-all |
| `t` | Toggle threaded/flat view |
| `/` | Open search |
| `m` | Toggle read/unread |
| `*` or `s` | Toggle star/flag |
| `d` | Mark deleted |
| `$` | Expunge |
| `C` | Copy message |
| `M` | Move message |
| `b` | Toggle mailbox sidebar |
| `[` / `]` | Previous/next account |

### Message detail view

| Key | Action |
|---|---|
| `↑/↓` or `j/k` | Scroll body |
| `Tab` | Next attachment |
| `s` | Save selected attachment |
| `H` | Open HTML body in browser |
| `Esc` or `q` | Back to message list |

### Compose

| Key | Action |
|---|---|
| `Tab` | Next field (To → Subject → Body) |
| `Enter` | Newline in body, next field otherwise |
| `Backspace` | Delete last character |
| `Ctrl+S` | Toggle PGP sign |
| `Ctrl+E` | Toggle PGP encrypt |
| `Ctrl+X` | Send |
| `Ctrl+D` | Save draft |
| `Esc` | Cancel |
| `a` | Open file browser (attach) |

### File browser

| Key | Action |
|---|---|
| `↑/↓` or `j/k` | Navigate files |
| `Enter` | Enter directory or select file |
| `Esc` | Cancel |

### Search

| Key | Action |
|---|---|
| `/` | Enter search mode |
| `Enter` | Run search |
| `Esc` | Exit search |

### Mailbox sidebar

| Key | Action |
|---|---|
| `b` | Toggle sidebar |
| `↑/↓` or `j/k` | Navigate mailboxes |
| `Enter` | Switch to selected mailbox (or confirm copy/move destination) |
| `n` | Create new mailbox |
| `D` | Delete selected mailbox (confirm: `y`/`n`) |

### Passphrase prompt

| Key | Action |
|---|---|
| Printable keys | Type passphrase (masked as `*`) |
| `Backspace` | Delete last character |
| `Enter` | Submit passphrase |
| `Esc` | Cancel (restores compose) |

## Copy / move messages

Press `C` or `M` on a message. The sidebar opens as a destination picker. Navigate to the target and press `Enter`.

- **Copy** leaves the original in place.
- **Move** copies to destination, marks original `\Deleted`, and expunges.

## Compose with PGP

**Signing:** Set `pgp_key_path` per account, toggle `Ctrl+S`. If the key is encrypted and not cached, an in-TUI passphrase prompt appears. Passphrase is cached in memory (zeroized on exit) for the session.

**Encryption:** Set `pgp_keyring_dir` for recipient public keys. Toggle `Ctrl+E`. Aborts with clear error if a recipient key is missing — never silently downgrades.

## OAuth2 / XOAUTH2

Set `imap_oauth2_token` to use XOAUTH2 instead of password auth. If `imap_oauth2_refresh_token`, `imap_oauth2_client_id`, and `imap_oauth2_token_endpoint` are also configured, expired tokens are automatically refreshed via RFC 6749 on auth failure.

## Limits

All configurable in `[imap_timeouts]` and `[search]`:

| Limit | Default | Eviction |
|---|---|---|
| Incoming message size | 25 MB | Refused before buffering |
| Cached messages/mailbox | 10 000 | Lowest UIDs removed |
| Search index entries | 50 000 | Oldest across accounts |
| Stored contacts | 5 000 | Least recently seen |

## HTML email

HTML-only messages converted to plaintext via `html2text`. Press `H` to open in browser — content is sanitized (scripts, iframes, remote images stripped) before writing.

## Logging

Set `RUST_LOG` to control log level (`error`, `warn`, `info`, `debug`). Default: `info`. Output goes to stderr with millisecond timestamps.

## Integration tests

```bash
./ci/start-greenmail.sh          # start test IMAP server
cargo test -p neomutt-mail-store --test integration_test -- --nocapture
```
