use std::fmt;

pub mod thread;
pub use thread::{thread_mailbox, thread_messages, ThreadNode};

// ---------------------------------------------------------------------------
// FlagSet — a compact bitset for IMAP/neomutt flags
// ---------------------------------------------------------------------------

bitflags::bitflags! {
    /// Mailbox message flags.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct FlagSet: u8 {
        const SEEN     = 1 << 0;
        const ANSWERED = 1 << 1;
        const FLAGGED  = 1 << 2;
        const DELETED  = 1 << 3;
        const DRAFT    = 1 << 4;
        const RECENT   = 1 << 5;
    }
}

// ---------------------------------------------------------------------------
// Envelope — header metadata extracted from a parsed email
// ---------------------------------------------------------------------------

/// Owned snapshot of the most important RFC 5322 / RFC 2047 headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub date: String,
    pub message_id: String,
    pub in_reply_to: String,
    pub references: String,
}

impl Envelope {
    /// Build an `Envelope` from a successfully-parsed `mail_parser::Message`.
    pub fn from_parsed(msg: &mail_parser::Message<'_>) -> Self {
        Self {
            subject: msg.subject().map(rfc2047_decode).unwrap_or_default(),
            from: msg.from().and_then(addr_to_string).unwrap_or_default(),
            to: msg.to().and_then(addr_to_string).unwrap_or_default(),
            date: msg.date().map(format_datetime).unwrap_or_default(),
            message_id: msg.message_id().unwrap_or("").to_owned(),
            in_reply_to: header_value_to_string(msg.in_reply_to()),
            references: header_value_to_string(msg.references()),
        }
    }
}

/// Walk the MIME tree of a parsed message and collect attachment metadata.
///
/// Returns an empty `Vec` for messages with no non-inline parts (e.g.
/// plain-text-only or multipart/alternative with just text+html).
pub fn parse_attachments(msg: &mail_parser::Message<'_>) -> Vec<Attachment> {
    msg.attachments()
        .map(|part| {
            let filename = part_filename(part).unwrap_or_else(|| "unnamed".into());
            let content_type = part_content_type(part).unwrap_or_else(|| "application/octet-stream".into());
            let size = part_size(part);
            Attachment {
                filename,
                content_type,
                size,
                body: None,
            }
        })
        .collect()
}

fn part_filename(part: &mail_parser::MessagePart<'_>) -> Option<String> {
    for h in &part.headers {
        let name = h.name.as_str().to_lowercase();
        if name == "content-disposition"
            && let mail_parser::HeaderValue::ContentType(ct) = &h.value
                && let Some(param) = ct.attribute("filename") {
                    return Some(param.to_string());
                }
        if name == "content-type"
            && let mail_parser::HeaderValue::ContentType(ct) = &h.value
                && let Some(param) = ct.attribute("name") {
                    return Some(param.to_string());
                }
    }
    None
}

fn part_content_type(part: &mail_parser::MessagePart<'_>) -> Option<String> {
    for h in &part.headers {
        if h.name.as_str().eq_ignore_ascii_case("content-type")
            && let mail_parser::HeaderValue::ContentType(ct) = &h.value {
                let subtype = ct.c_subtype.as_deref().unwrap_or("octet-stream");
                return Some(format!("{}/{}", ct.c_type, subtype));
            }
    }
    None
}

fn part_size(part: &mail_parser::MessagePart<'_>) -> usize {
    match &part.body {
        mail_parser::PartType::Text(t) => t.len(),
        mail_parser::PartType::Html(h) => h.len(),
        mail_parser::PartType::Binary(b) => b.len(),
        mail_parser::PartType::InlineBinary(b) => b.len(),
        _ => 0,
    }
}

/// Extract the raw HTML body from a message if a text/html part exists.
pub fn parse_html_body(raw: &[u8]) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    let html_parts: Vec<String> = parsed
        .html_bodies()
        .filter_map(|part| match &part.body {
            mail_parser::PartType::Html(h) => Some(h.as_ref().to_owned()),
            _ => None,
        })
        .collect();
    if html_parts.is_empty() {
        None
    } else {
        Some(html_parts.join("<br>"))
    }
}

/// Parse the text body from a raw RFC 2822 message.
///
/// Prefers `text/plain` parts.  Falls back to `text/html` parts converted
/// to plaintext via `html2text` when no usable `text/plain` part exists.
pub fn parse_body_text(raw: &[u8]) -> String {
    parse_body_text_with_width(raw, 80)
}

/// Parse body text with a configurable HTML-to-text wrap width.
pub fn parse_body_text_with_width(raw: &[u8], wrap_width: usize) -> String {
    let parsed = mail_parser::MessageParser::default().parse(raw);
    let Some(msg) = parsed else {
        return String::new();
    };

    // Collect text/plain parts.
    let text_parts: Vec<String> = msg
        .text_bodies()
        .filter_map(|part| match &part.body {
            mail_parser::PartType::Text(t) => Some(t.as_ref().to_owned()),
            _ => None,
        })
        .collect();

    if !text_parts.is_empty() {
        return text_parts.join("\n");
    }

    // No text/plain — try HTML parts.
    let html_parts: Vec<String> = msg
        .html_bodies()
        .filter_map(|part| match &part.body {
            mail_parser::PartType::Html(h) => Some(h.as_ref().to_owned()),
            _ => None,
        })
        .collect();

    if !html_parts.is_empty() {
        let html = html_parts.join("<br>");
        return html2text::from_read(html.as_bytes(), wrap_width)
            .unwrap_or_else(|_| "[HTML conversion failed]".into());
    }

    String::new()
}

/// Extract the plain-text content of a [`mail_parser::HeaderValue`].
fn header_value_to_string(hv: &mail_parser::HeaderValue<'_>) -> String {
    match hv {
        mail_parser::HeaderValue::Text(t) => t.to_string(),
        mail_parser::HeaderValue::TextList(list) => {
            list.iter().map(|c| c.as_ref()).collect::<Vec<_>>().join(" ")
        }
        _ => String::new(),
    }
}

/// Decode RFC 2047 encoded-words (e.g. `=?UTF-8?B?...?=`) in a header value.
fn rfc2047_decode(raw: &str) -> String {
    rfc2047_decoder::decode(raw).unwrap_or_else(|_| raw.to_owned())
}

/// Convert a `mail_parser::Address` to a plain `String`, preferring the
/// display-name when available.
fn addr_to_string(addr: &mail_parser::Address<'_>) -> Option<String> {
    match addr {
        mail_parser::Address::List(list) => {
            let parts: Vec<String> = list.iter().filter_map(addr_single_to_string).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        mail_parser::Address::Group(groups) => {
            let parts: Vec<String> = groups
                .iter()
                .flat_map(|g| {
                    let addrs: Vec<String> =
                        g.addresses.iter().filter_map(addr_single_to_string).collect();
                    match (&g.name, addrs.is_empty()) {
                        (_, true) => vec![],
                        (Some(name), false) => vec![format!("{}: {};", name, addrs.join(", "))],
                        (None, false) => addrs,
                    }
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
    }
}

/// Format a `mail_parser::DateTime` as an RFC 5322 date string.
fn format_datetime(dt: &mail_parser::DateTime) -> String {
    let tz_sign = if dt.tz_before_gmt { "-" } else { "+" };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}{:02}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, tz_sign, dt.tz_hour, dt.tz_minute
    )
}

/// Format a single `mail_parser::Addr` as `Display Name <address>` or just
/// `<address>`.
fn addr_single_to_string(a: &mail_parser::Addr<'_>) -> Option<String> {
    match (&a.name, &a.address) {
        (Some(name), Some(addr)) => Some(format!("{} <{}>", name, addr)),
        (None, Some(addr)) => Some(addr.to_string()),
        (Some(name), None) => Some(name.to_string()),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Attachment — non-inline MIME part metadata
// ---------------------------------------------------------------------------

/// Metadata for a single attachment (non-inline MIME part).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// Suggested filename from Content-Disposition or Content-Type.
    pub filename: String,
    /// MIME content-type (e.g. "application/pdf").
    pub content_type: String,
    /// Size in bytes of the encoded body.
    pub size: usize,
    /// The raw attachment bytes, if fetched.  Lazy — initially `None`.
    pub body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Message — a single message in a mailbox
// ---------------------------------------------------------------------------

/// A message known to the local mailbox model.
///
/// `uid` is the server-assigned unique identifier.  `body_fetched` tracks
/// whether the full body has been downloaded (as opposed to headers-only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub uid: u32,
    pub envelope: Envelope,
    pub flags: FlagSet,
    pub body_fetched: bool,
    /// The decoded body text.  Empty until fetched on demand.
    pub body: String,
    /// The raw HTML body, if the message had a text/html part.
    /// Preserved for external browser viewing.
    pub html_body: Option<String>,
    /// Non-inline attachments discovered during MIME parsing.
    pub attachments: Vec<Attachment>,
}

impl Message {
    pub fn new(uid: u32, envelope: Envelope, flags: FlagSet) -> Self {
        Self {
            uid,
            envelope,
            flags,
            body_fetched: false,
            body: String::new(),
            html_body: None,
            attachments: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Mailbox — ordered collection of messages
// ---------------------------------------------------------------------------

/// The single source of truth for the mailbox contents.
///
/// Owned exclusively by the App State task; other tasks receive snapshots or
/// deltas via channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailbox {
    pub messages: Vec<Message>,
}

impl Mailbox {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Count messages whose `SEEN` flag is **not** set.
    pub fn unseen_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| !m.flags.contains(FlagSet::SEEN))
            .count()
    }
}

impl fmt::Display for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "From: {}\nTo: {}\nDate: {}\nSubject: {}",
            self.from, self.to, self.date, self.subject
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flagset_default_is_empty() {
        let flags = FlagSet::default();
        assert!(!flags.contains(FlagSet::SEEN));
        assert!(!flags.contains(FlagSet::DELETED));
    }

    #[test]
    fn flagset_combine() {
        let flags = FlagSet::SEEN | FlagSet::FLAGGED;
        assert!(flags.contains(FlagSet::SEEN));
        assert!(flags.contains(FlagSet::FLAGGED));
        assert!(!flags.contains(FlagSet::ANSWERED));
    }

    #[test]
    fn mailbox_new_is_empty() {
        let mb = Mailbox::new();
        assert!(mb.is_empty());
        assert_eq!(mb.len(), 0);
    }

    #[test]
    fn unseen_count_counts_unseen_only() {
        let env = Envelope {
            subject: "test".into(),
            from: "a@b".into(),
            to: "c@d".into(),
            date: "now".into(),
            message_id: "id".into(),
            in_reply_to: String::new(),
            references: String::new(),
        };
        let seen = Message::new(1, env.clone(), FlagSet::SEEN);
        let unseen = Message::new(2, env, FlagSet::default());
        let mb = Mailbox {
            messages: vec![seen, unseen],
        };
        assert_eq!(mb.unseen_count(), 1);
    }
}
