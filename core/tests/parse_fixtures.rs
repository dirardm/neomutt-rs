use mail_parser::MessageParser;
use neomutt_core::{parse_attachments, parse_body_text, parse_body_text_with_width, Envelope, FlagSet, Mailbox, Message};

/// Read a fixture file, parse it, and return the raw bytes.
fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("tests/fixtures/{}", name);
    std::fs::read(&path).expect("failed to read fixture file")
}

/// Parse raw bytes through `mail_parser`, then convert to our `Message`.
fn parse_message(raw: &[u8], uid: u32) -> Message {
    let parsed = MessageParser::default()
        .parse(raw)
        .expect("mail-parser should succeed on valid .eml");
    let envelope = Envelope::from_parsed(&parsed);
    let attachments = parse_attachments(&parsed);
    let mut msg = Message::new(uid, envelope, FlagSet::default());
    msg.attachments = attachments;
    msg
}

// ---------------------------------------------------------------------------
// Plain text email
// ---------------------------------------------------------------------------

#[test]
fn plain_text_headers_are_extracted() {
    let raw = load_fixture("plain-text.eml");
    let msg = parse_message(&raw, 1);

    assert_eq!(msg.uid, 1);
    assert_eq!(msg.envelope.subject, "Hello from the other side");
    assert!(msg.envelope.from.contains("alice@example.com"));
    assert!(msg.envelope.from.contains("Alice"));
    assert!(msg.envelope.to.contains("bob@example.com"));
    // mail-parser strips angle brackets from Message-ID
    assert_eq!(
        msg.envelope.message_id,
        "20240115103000.0001@example.com"
    );
    assert!(!msg.body_fetched);
}

// ---------------------------------------------------------------------------
// Multipart/alternative email
// ---------------------------------------------------------------------------

#[test]
fn multipart_alternative_headers_are_extracted() {
    let raw = load_fixture("multipart-alternative.eml");
    let msg = parse_message(&raw, 42);

    assert_eq!(msg.uid, 42);
    assert_eq!(msg.envelope.subject, "February updates");
    assert!(msg.envelope.from.contains("Newsletter"));
    assert!(msg.envelope.from.contains("news@example.org"));
    assert_eq!(msg.envelope.to, "user@example.com");
    // mail-parser strips angle brackets from Message-ID
    assert_eq!(
        msg.envelope.message_id,
        "20240220140000.0002@example.org"
    );
    assert!(!msg.body_fetched);
}

// ---------------------------------------------------------------------------
// RFC 2047 encoded headers
// ---------------------------------------------------------------------------

#[test]
fn rfc2047_encoded_headers_are_decoded() {
    let raw = load_fixture("rfc2047-encoded.eml");
    let msg = parse_message(&raw, 3);

    // mail-parser handles RFC 2047 decoding automatically, so these should
    // come out as plain UTF-8.
    assert!(msg.envelope.subject.contains("Hola"), "subject = {:?}", msg.envelope.subject);
    assert!(msg.envelope.from.contains("José"), "from = {:?}", msg.envelope.from);
    assert!(msg.envelope.to.contains("Müller"), "to = {:?}", msg.envelope.to);
}

// ---------------------------------------------------------------------------
// Mailbox bulk test
// ---------------------------------------------------------------------------

#[test]
fn mailbox_holds_multiple_messages() {
    let mut mb = Mailbox::new();

    for (i, file) in ["plain-text.eml", "multipart-alternative.eml", "rfc2047-encoded.eml"]
        .iter()
        .enumerate()
    {
        let raw = load_fixture(file);
        mb.messages.push(parse_message(&raw, i as u32 + 1));
    }

    assert_eq!(mb.len(), 3);
    assert_eq!(mb.messages[0].uid, 1);
    assert_eq!(mb.messages[1].uid, 2);
    assert_eq!(mb.messages[2].uid, 3);
}

// ---------------------------------------------------------------------------
// Attachment parsing
// ---------------------------------------------------------------------------

#[test]
fn attachment_parsed_correctly() {
    let raw = load_fixture("with-attachment.eml");
    let msg = parse_message(&raw, 1);

    assert_eq!(msg.attachments.len(), 1, "should have one attachment");
    let att = &msg.attachments[0];
    assert_eq!(att.filename, "report.pdf");
    assert_eq!(att.content_type, "application/pdf");
    assert!(att.size > 0, "attachment should have non-zero size");
    assert!(att.body.is_none(), "body not fetched yet");
}

#[test]
fn multipart_alternative_has_no_attachments() {
    let raw = load_fixture("multipart-alternative.eml");
    let msg = parse_message(&raw, 1);

    assert!(
        msg.attachments.is_empty(),
        "multipart/alternative with text+html should have no attachments"
    );
}

// ---------------------------------------------------------------------------
// Body parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_body_text_extracts_plain_text() {
    let raw = load_fixture("plain-text.eml");
    let body = parse_body_text(&raw);

    assert!(body.contains("Hey Bob"));
    assert!(body.contains("Just checking in"));
    assert!(!body.is_empty());
}

#[test]
fn parse_body_text_returns_empty_for_empty_input() {
    let body = parse_body_text(b"");
    assert!(body.is_empty());
}

#[test]
fn html_only_body_is_converted_to_plaintext() {
    let raw = load_fixture("html-only.eml");
    let body = parse_body_text(&raw);

    // Should contain readable text from the HTML, not raw tags.
    assert!(body.contains("Welcome"));
    assert!(body.contains("HTML-only"));
    assert!(body.contains("Item one"));
    assert!(body.contains("Item two"));
    assert!(!body.contains("<html>"), "should not contain raw HTML tags");
    assert!(!body.contains("<h1>"), "should not contain raw HTML tags");
}

#[test]
fn parse_body_text_with_width_respects_wrap_parameter() {
    // parse_body_text uses width 80 by default.
    let default_body = parse_body_text(b"");
    // parse_body_text_with_width lets callers override.
    let custom_body = parse_body_text_with_width(b"", 40);

    // Both produce empty for empty input.
    assert!(default_body.is_empty());
    assert!(custom_body.is_empty());

    // For a real HTML input with long lines, the wrap width matters.
    let raw = load_fixture("html-only.eml");
    let wide = parse_body_text_with_width(&raw, 200);
    let narrow = parse_body_text_with_width(&raw, 20);

    // Narrow wrapping should produce shorter lines than wide.
    let wide_max = wide.lines().map(|l| l.len()).max().unwrap_or(0);
    let narrow_max = narrow.lines().map(|l| l.len()).max().unwrap_or(0);
    assert!(
        narrow_max <= 25,
        "narrow wrap (20) should bound lines; max was {narrow_max}"
    );
    assert!(
        wide_max > narrow_max,
        "wide wrap (200) should allow longer lines than narrow (20)"
    );
}
