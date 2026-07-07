# mail-store integration tests

End-to-end tests against a real IMAP server (Greenmail). These tests verify actual IMAP protocol behavior, not just mocked command construction.

## Quick start

```bash
# Start the test server
./ci/start-greenmail.sh

# Run the integration tests
cargo test -p neomutt-mail-store --test integration_test -- --nocapture

# Run all workspace tests (unit + integration)
cargo test --workspace
```

## Server setup

Greenmail is a lightweight Java IMAP/SMTP test server designed for integration testing. It provides:

- **IMAP** on `localhost:3143` (plain, no TLS — for local testing)
- **SMTP** on `localhost:3025` (used by the IDLE notification test)

Credentials: `testuser` / `testpass` and `testuser2` / `testpass2`.

The server is started via `ci/start-greenmail.sh` which downloads and runs the standalone JAR (version 2.1.6).

## Test coverage

| Test | What it exercises |
|---|---|
| `connect_list_select_fetch_happy_path` | Connect → LIST → CREATE mailbox → APPEND message → SELECT + FETCH → verify parsed Message data |
| `flag_set_and_clear_round_trip` | STORE +FLAGS (SEEN) → FETCH verify → STORE -FLAGS → FETCH verify cleared |
| `copy_message_between_mailboxes` | CREATE src/dst → APPEND → UID COPY → verify dst has message, src still has it |
| `move_message_fallback_works` | CREATE src/dst → APPEND → COPY + STORE \Deleted + EXPUNGE → verify dst has it, src doesn't |
| `append_draft_appears_in_target_mailbox` | APPEND with \Draft flag → FETCH → verify message appears |
| `idle_notify_then_fetch` | Spawn idle_loop → inject via SMTP → verify MailboxUpdated event fires |
| `starttls_upgrade_works_end_to_end` | Placeholder — Greenmail doesn't advertise STARTTLS |

## Env vars

| Variable | Default | Description |
|---|---|---|
| `NEOMUTT_TEST_IMAP_HOST` | `localhost` | IMAP server host |
| `NEOMUTT_TEST_IMAP_PORT` | `3143` | IMAP server port |
| `NEOMUTT_TEST_IMAP_USER` | `testuser` | IMAP username |
| `NEOMUTT_TEST_IMAP_PASS` | `testpass` | IMAP password |
| `NEOMUTT_TEST_SMTP_HOST` | `localhost` | SMTP server host (for IDLE test) |
| `NEOMUTT_TEST_SMTP_PORT` | `3025` | SMTP server port |
