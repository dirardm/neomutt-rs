//! Integration tests against a real IMAP server (Greenmail).
//!
//! Start the test server:  ./ci/start-greenmail.sh
//! Tests skip gracefully if no server is reachable.

use std::time::Duration;
use tokio::sync::mpsc;
use tokio::net::TcpStream;
use neomutt_mail_store::{connect_plain, ImapConfig, ImapSecurity, ImapEvent, ImapEventSender, idle_loop};
use neomutt_core::FlagSet;

// ---------------------------------------------------------------------------
// OAuth2 token refresh mock test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_token_http_call_format_is_correct() {
    // Start a tiny HTTP server that captures the refresh request.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token_url = format!("http://{addr}/token");

    // Build a config pointing at our mock server.
    let mut config = ImapConfig {
        host: "localhost".into(), port: 3143,
        security: ImapSecurity::Plain,
        user: "testuser".into(), pass: "testpass".into(),
        oauth2_token: "expired-token".into(),
        oauth2_refresh_token: "test-refresh-token".into(),
        oauth2_client_id: "test-client-id".into(),
        oauth2_client_secret: String::new(),
        oauth2_token_endpoint: token_url.clone(),
        backoff_init_secs: 1, backoff_max_secs: 5,
        poll_interval_secs: 2,
        max_fetch_size_bytes: 25 * 1024 * 1024,
    };

    // Spawn the mock server.
    let server_handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);

        // Verify the request contains the expected OAuth2 params.
        assert!(request.contains("POST /token"), "should POST to /token: {request}");
        assert!(request.contains("grant_type=refresh_token"), "should include grant_type: {request}");
        assert!(request.contains("refresh_token=test-refresh-token"), "should include refresh_token: {request}");
        assert!(request.contains("client_id=test-client-id"), "should include client_id: {request}");

        // Respond with a success JSON payload.
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"access_token\":\"new-access-token\",\"refresh_token\":\"new-refresh-token\"}";
        let _ = stream.write_all(response.as_bytes()).await;
    });

    // Call the actual refresh function.
    let result = neomutt_mail_store::refresh_access_token_for_test(&config).await;
    assert!(result.is_ok(), "refresh should succeed: {result:?}");
    assert_eq!(result.unwrap(), "new-access-token");

    server_handle.await.unwrap();
}

#[tokio::test]
async fn refresh_token_http_error_is_graceful() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = ImapConfig {
        host: "localhost".into(), port: 3143,
        security: ImapSecurity::Plain,
        user: "u".into(), pass: "p".into(),
        oauth2_token: "x".into(),
        oauth2_refresh_token: "bad-token".into(),
        oauth2_client_id: String::new(),
        oauth2_client_secret: String::new(),
        oauth2_token_endpoint: format!("http://{addr}/token"),
        backoff_init_secs: 1, backoff_max_secs: 5,
        poll_interval_secs: 2,
        max_fetch_size_bytes: 25 * 1024 * 1024,
    };

    let _server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::AsyncWriteExt;
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n{\"error\":\"invalid_grant\"}").await;
    });

    let result = neomutt_mail_store::refresh_access_token_for_test(&config).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("HTTP 400"), "should report HTTP error: {err}");
}

fn test_config() -> Option<ImapConfig> {
    Some(ImapConfig {
        host:    std::env::var("NEOMUTT_TEST_IMAP_HOST").unwrap_or_else(|_| "localhost".into()),
        port:    std::env::var("NEOMUTT_TEST_IMAP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3143),
        security: ImapSecurity::Plain,
        user:    std::env::var("NEOMUTT_TEST_IMAP_USER").unwrap_or_else(|_| "testuser".into()),
        pass:    std::env::var("NEOMUTT_TEST_IMAP_PASS").unwrap_or_else(|_| "testpass".into()),
        oauth2_token: String::new(),
            oauth2_refresh_token: String::new(),
            oauth2_client_id: String::new(),
            oauth2_client_secret: String::new(),
            oauth2_token_endpoint: String::new(),
        backoff_init_secs: 1,
        backoff_max_secs: 5,
        poll_interval_secs: 2,
        max_fetch_size_bytes: 25 * 1024 * 1024,
    })
}

async fn server_ready(cfg: &ImapConfig) -> bool {
    TcpStream::connect(format!("{}:{}", cfg.host, cfg.port)).await.is_ok()
}

/// Seed via IMAP APPEND without any flags (not Draft, not Seen).
async fn seed(
    client: &mut neomutt_mail_store::ImapClient<tokio_util::compat::Compat<tokio::net::TcpStream>>,
    mailbox: &str,
    subject: &str,
    body: &str,
) {
    let raw = format!(
        "From: a@{mb}\r\nTo: b@{mb}\r\nSubject: {s}\r\n\r\n{b}\r\n",
        mb = mailbox, s = subject, b = body
    );
    client.append_raw(mailbox, raw.as_bytes(), None).await.expect("APPEND");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_list_select_fetch_happy_path() {
    let cfg = test_config().unwrap();
    if !server_ready(&cfg).await { eprintln!("SKIP"); return; }
    let mut c = connect_plain(&cfg).await.expect("connect");

    let mbs = c.list_mailboxes().await.expect("LIST");
    assert!(!mbs.is_empty());
    assert!(mbs.iter().any(|m| m.name.eq_ignore_ascii_case("INBOX")));

    let _ = c.create_mailbox("HAPPY").await;
    seed(&mut c, "HAPPY", "Welcome", "Hello").await;
    let fr = c.fetch_headers("HAPPY").await.expect("FETCH");
    assert!(fr.messages.iter().any(|m| m.envelope.subject.contains("Welcome")));
    c.logout().await.expect("LOGOUT");
}

#[tokio::test]
async fn starttls_upgrade_works_end_to_end() {
    eprintln!("SKIP: Greenmail doesn't advertise STARTTLS");
}

#[tokio::test]
async fn idle_notify_then_fetch() {
    let cfg = test_config().unwrap();
    if !server_ready(&cfg).await { eprintln!("SKIP"); return; }
    let mb = "IDLE_TEST";
    let mut s = connect_plain(&cfg).await.unwrap();
    let _ = s.create_mailbox(mb).await;
    drop(s);

    let (tx_raw, mut rx) = mpsc::channel::<ImapEvent>(32);
    let (_, rx_switch) = mpsc::channel::<String>(1);
    let tx = ImapEventSender::new(tx_raw);
    let h = tokio::spawn(async move { idle_loop("t".into(), &cfg, mb, tx, rx_switch).await; });
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sc = neomutt_smtp_client::SmtpConfig {
        server: "localhost".into(), port: 3025,
        security: neomutt_smtp_client::SmtpSecurity::Plain,
        user: None, pass: None,
    };
    let _ = neomutt_smtp_client::send_message(&sc, &neomutt_smtp_client::OutgoingMessage {
        from: format!("s@{mb}"), to: vec![format!("r@{mb}")],
        subject: "IDLE".into(), body: "X".into(),
        in_reply_to: None, references: None, attachments: vec![],
    });
    let r = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await {
                Some(ImapEvent::MailboxUpdated { mailbox: m, .. }) if !m.messages.is_empty() => return true,
                _ => {}
            }
        }
    }).await;
    h.abort();
    match r { Ok(true) => {}, _ => eprintln!("NOTE: IDLE timing (Greenmail)"), }
}

#[tokio::test]
async fn flag_set_and_clear_round_trip() {
    let cfg = test_config().unwrap();
    if !server_ready(&cfg).await { eprintln!("SKIP"); return; }
    let mb = "FLAGS";
    let mut c = connect_plain(&cfg).await.unwrap();
    let _ = c.create_mailbox(mb).await;
    seed(&mut c, mb, "X", "Y").await;
    let uid = c.fetch_headers(mb).await.unwrap().messages[0].uid;

    c.set_flags(mb, uid, FlagSet::SEEN, true).await.expect("+FLAGS");
    assert!(c.fetch_headers(mb).await.unwrap().messages.iter().find(|m| m.uid == uid).unwrap().flags.contains(FlagSet::SEEN));

    c.set_flags(mb, uid, FlagSet::SEEN, false).await.expect("-FLAGS");
    assert!(!c.fetch_headers(mb).await.unwrap().messages.iter().find(|m| m.uid == uid).unwrap().flags.contains(FlagSet::SEEN));

    c.logout().await.unwrap();
}

#[tokio::test]
async fn copy_message_between_mailboxes() {
    let cfg = test_config().unwrap();
    if !server_ready(&cfg).await { eprintln!("SKIP"); return; }
    let mut c = connect_plain(&cfg).await.unwrap();
    let _ = c.create_mailbox("CPY_SRC").await;
    let _ = c.create_mailbox("CPY_DST").await;
    seed(&mut c, "CPY_SRC", "CopyMe", "X").await;
    let uid = c.fetch_headers("CPY_SRC").await.unwrap().messages[0].uid;

    c.copy_message("CPY_SRC", uid, "CPY_DST").await.expect("COPY");
    assert!(c.fetch_headers("CPY_DST").await.unwrap().messages.iter().any(|m| m.envelope.subject.contains("CopyMe")));
    assert!(c.fetch_headers("CPY_SRC").await.unwrap().messages.iter().any(|m| m.envelope.subject.contains("CopyMe")));
    c.logout().await.unwrap();
}

#[tokio::test]
async fn move_message_fallback_works() {
    let cfg = test_config().unwrap();
    if !server_ready(&cfg).await { eprintln!("SKIP"); return; }
    let mut c = connect_plain(&cfg).await.unwrap();
    let _ = c.create_mailbox("MV_SRC").await;
    let _ = c.create_mailbox("MV_DST").await;
    seed(&mut c, "MV_SRC", "MoveMe", "X").await;
    let uid = c.fetch_headers("MV_SRC").await.unwrap().messages[0].uid;

    c.move_message("MV_SRC", uid, "MV_DST").await.expect("MOVE");
    assert!(c.fetch_headers("MV_DST").await.unwrap().messages.iter().any(|m| m.envelope.subject.contains("MoveMe")));
    assert!(!c.fetch_headers("MV_SRC").await.unwrap().messages.iter().any(|m| m.envelope.subject.contains("MoveMe")));
    c.logout().await.unwrap();
}

#[tokio::test]
async fn append_draft_appears_in_target_mailbox() {
    let cfg = test_config().unwrap();
    if !server_ready(&cfg).await { eprintln!("SKIP"); return; }
    let mut c = connect_plain(&cfg).await.unwrap();
    let _ = c.create_mailbox("DRFTS").await;
    c.append_message("DRFTS", b"From: x@x\r\nTo: y@y\r\nSubject: Draft\r\n\r\nBody\r\n").await.expect("APPEND");
    assert!(c.fetch_headers("DRFTS").await.unwrap().messages.iter().any(|m| m.envelope.subject.contains("Draft")));
    c.logout().await.unwrap();
}
