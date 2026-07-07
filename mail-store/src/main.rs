//! Smoke-test binary for the IMAP client.
//!
//! Connects using the standard env vars, lists mailboxes, and fetches headers
//! from INBOX (or `IMAP_MAILBOX`).  Prints each message's envelope.
//!
//! If `IMAP_HOST` is not set the binary exits 0 immediately — safe for CI.
//!
//! ```text
//! IMAP_HOST=imap.example.com IMAP_USER=me IMAP_PASS=secret cargo run -p neomutt-mail-store
//! ```

use std::time::Duration;

use tokio::sync::mpsc;

use neomutt_mail_store::{ImapClient, ImapStream};

#[tokio::main]
async fn main() {
    // Gate — skip gracefully when no server is configured.
    if std::env::var("IMAP_HOST").is_err() {
        log::info!("SKIP: IMAP_HOST not set — no live IMAP server to test against.");
        return;
    }

    let config = match neomutt_mail_store::ImapConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            log::info!("SKIP: {e}");
            return;
        }
    };

    let target_mailbox =
        std::env::var("IMAP_MAILBOX").unwrap_or_else(|_| "INBOX".to_owned());

    println!("Connecting to {}:{} …", config.host, config.port);
    let mut client = match ImapClient::<ImapStream>::connect(&config).await {
        Ok(c) => c,
        Err(e) => {
            log::error!("FATAL: failed to connect — {e}");
            std::process::exit(1);
        }
    };

    // -- list mailboxes -------------------------------------------------------
    match client.list_mailboxes().await {
        Ok(entries) => {
            println!("\n{} mailboxes found:", entries.len());
            for entry in &entries {
                println!("  {}  ({})", entry.label, entry.name);
            }
        }
        Err(e) => log::error!("list mailboxes failed: {e}"),
    }

    // -- fetch headers from the target mailbox --------------------------------
    println!("\nFetching headers from {target_mailbox} …");
    match client.fetch_headers(&target_mailbox).await {
        Ok(fr) => {
            println!(
                "  UIDVALIDITY={:?}, {} messages:",
                fr.uid_validity,
                fr.messages.len()
            );
            for msg in &fr.messages {
                println!(
                    "  ┌─ UID {}  flags {:?}",
                    msg.uid, msg.flags
                );
                println!("  │ {}", msg.envelope);
                println!("  └");
            }
        }
        Err(e) => log::error!("fetch failed: {e}"),
    }

    // -- poll a few iterations ------------------------------------------------
    println!("\nPolling every 2 s (press Ctrl-C to stop) …");
    // In a real app this runs forever; here we just show it works.
    let (tx_raw, _rx) = mpsc::channel::<neomutt_mail_store::ImapEvent>(16);
    let tx = neomutt_mail_store::ImapEventSender::new(tx_raw);
    let _ = tokio::time::timeout(
        Duration::from_secs(8),
        client.poll_loop(&target_mailbox, Duration::from_secs(2), &tx, "test"),
    )
    .await;

    if let Err(e) = client.logout().await {
        log::error!("logout error: {e}");
    }
    println!("Done.");
}
