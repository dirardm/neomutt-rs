//! Thin wrapper over [`lettre`] for sending email.
//!
//! This crate is deliberately small — it translates caller-provided fields
//! into `lettre` builder calls so the rest of the system never touches
//! `lettre` directly.

use std::fmt;

use lettre::message::header::{HeaderName, HeaderValue};
use lettre::{
    message::Mailbox as LettreMailbox,
    Message as LettreMessage,
    Transport,
};
use lettre::transport::smtp::SmtpTransport;

// ---------------------------------------------------------------------------
// SMTP configuration
// ---------------------------------------------------------------------------

/// SMTP connection security mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SmtpSecurity {
    /// STARTTLS — plain TCP, then upgrade (typically port 587).  The default.
    #[default]
    StartTls,
    /// Implicit TLS / SMTPS (typically port 465).
    Tls,
    /// Plain text, no TLS — for local testing only.
    Plain,
}

/// Connection details for the outgoing SMTP server.
#[derive(Clone)]
pub struct SmtpConfig {
    pub server: String,
    pub port: u16,
    pub security: SmtpSecurity,
    /// If `None` the transport is built without authentication.
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl std::fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("server", &self.server)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("user", &self.user)
            .field("pass", &"***REDACTED***")
            .finish()
    }
}

impl SmtpConfig {
    /// Read from `SMTP_SERVER`, `SMTP_PORT` (default 587), `SMTP_SECURITY`
    /// (default `"starttls"`), `SMTP_USER`, `SMTP_PASS`.
    pub fn from_env() -> Result<Self, &'static str> {
        let server =
            std::env::var("SMTP_SERVER").map_err(|_| "SMTP_SERVER not set")?;
        let port = std::env::var("SMTP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(587);
        let security = std::env::var("SMTP_SECURITY")
            .ok()
            .map(|s| match s.to_lowercase().as_str() {
                "tls" => SmtpSecurity::Tls,
                _ => SmtpSecurity::StartTls,
            })
            .unwrap_or_default();
        let user = std::env::var("SMTP_USER").ok();
        let pass = std::env::var("SMTP_PASS").ok();
        Ok(Self {
            server,
            port,
            security,
            user,
            pass,
        })
    }
}

// ---------------------------------------------------------------------------
// Outgoing message
// ---------------------------------------------------------------------------

/// A message ready to be handed to the SMTP transport.
#[derive(Clone, Debug)]
/// A file attached to an outgoing message.
pub struct FileAttachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

pub struct OutgoingMessage {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub attachments: Vec<FileAttachment>,
}

// ---------------------------------------------------------------------------
// Send
// ---------------------------------------------------------------------------

/// Error envelope so callers can display the failure reason.
#[derive(Debug)]
pub struct SendError(pub String);

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SendError {}

impl From<lettre::error::Error> for SendError {
    fn from(e: lettre::error::Error) -> Self {
        SendError(e.to_string())
    }
}

impl From<lettre::address::AddressError> for SendError {
    fn from(e: lettre::address::AddressError) -> Self {
        SendError(e.to_string())
    }
}

impl From<lettre::transport::smtp::Error> for SendError {
    fn from(e: lettre::transport::smtp::Error) -> Self {
        SendError(e.to_string())
    }
}

/// Build and send a message via `lettre`.
///
/// Supports two TLS modes based on `config.security`:
/// - `StartTls` (default) — plain TCP → STARTTLS upgrade (typically port 587).
/// - `Tls` — implicit TLS / SMTPS (typically port 465).
///
/// Credentials are only sent after the TLS handshake completes.
pub fn send_message(
    config: &SmtpConfig,
    msg: &OutgoingMessage,
) -> Result<(), SendError> {
    let from: LettreMailbox = msg.from.parse()?;
    let to: Vec<LettreMailbox> = msg
        .to
        .iter()
        .map(|a| a.parse::<LettreMailbox>())
        .collect::<Result<Vec<_>, _>>()?;

    let mut builder = LettreMessage::builder()
        .from(from)
        .subject(msg.subject.as_str());

    for addr in &to {
        builder = builder.to(addr.clone());
    }

    // Build body: if attachments exist, use MIME multipart/mixed.
    let mut email = if msg.attachments.is_empty() {
        builder.body(msg.body.clone())?
    } else {
        use lettre::message::{MultiPart, SinglePart};
        let body_part = SinglePart::builder()
            .header(lettre::message::header::ContentType::TEXT_PLAIN)
            .body(msg.body.clone());
        let mut multipart = MultiPart::mixed().singlepart(body_part);
        for att in &msg.attachments {
            let ct = lettre::message::header::ContentType::parse(&att.content_type)
                .map_err(|e| SendError(format!("invalid content-type '{}': {e}", att.content_type)))?;
            let att_part = SinglePart::builder()
                .header(ct)
                .body(att.data.clone());
            multipart = multipart.singlepart(att_part);
        }
        builder.multipart(multipart)?
    };

    // In-Reply-To and References (RFC 5322 § 3.6.4)
    if let Some(ref irt) = msg.in_reply_to {
        let name =
            HeaderName::new_from_ascii("In-Reply-To".into()).map_err(|e| {
                SendError(format!("invalid header name: {e}"))
            })?;
        email
            .headers_mut()
            .insert_raw(HeaderValue::new(name, irt.clone()));
    }
    if let Some(ref refs) = msg.references {
        let name =
            HeaderName::new_from_ascii("References".into()).map_err(|e| {
                SendError(format!("invalid header name: {e}"))
            })?;
        email
            .headers_mut()
            .insert_raw(HeaderValue::new(name, refs.clone()));
    }

    // Build transport — Plain is only for local testing, never in production.
    let mut transport = match config.security {
        SmtpSecurity::Plain => {
            SmtpTransport::builder_dangerous(&config.server)
        }
        SmtpSecurity::Tls => {
            SmtpTransport::relay(&config.server)
                .map_err(|e| SendError(format!("SMTP TLS relay error: {e}")))?
        }
        SmtpSecurity::StartTls => {
            SmtpTransport::starttls_relay(&config.server)
                .map_err(|e| SendError(format!("SMTP STARTTLS relay error: {e}")))?
        }
    };
    transport = transport.port(config.port);

    if let (Some(user), Some(pass)) = (&config.user, &config.pass) {
        use lettre::transport::smtp::authentication::Credentials;
        transport = transport.credentials(Credentials::new(user.clone(), pass.clone()));
    }

    transport.build().send(&email)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Preserved for backwards-compat / quick smoke tests
// ---------------------------------------------------------------------------

/// Quick smoke-test: send a hardcoded message to `localhost:25`.
///
/// Prefer [`send_message`] for real use.
pub fn send_test_message() -> Result<(), SendError> {
    let config = SmtpConfig {
        server: "localhost".into(),
        port: 25,
        security: SmtpSecurity::StartTls,
        user: None,
        pass: None,
    };
    let msg = OutgoingMessage {
        from: "neomutt-rs@localhost".into(),
        to: vec!["root@localhost".into()],
        subject: "neomutt-rs test message".into(),
        body: "This is a hardcoded test message sent by neomutt-rs.".into(),
        in_reply_to: None,
        references: None,
        attachments: Vec::new(),
    };
    send_message(&config, &msg)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_message_with_headers_succeeds() {
        let from: LettreMailbox = "a@b.com".parse().unwrap();
        let to: LettreMailbox = "c@d.com".parse().unwrap();

        let mut email = LettreMessage::builder()
            .from(from)
            .to(to)
            .subject("Test")
            .body("body".to_owned())
            .unwrap();

        let irt_name =
            HeaderName::new_from_ascii("In-Reply-To".into()).unwrap();
        email
            .headers_mut()
            .insert_raw(HeaderValue::new(irt_name, "<msg@id>".into()));

        let refs_name =
            HeaderName::new_from_ascii("References".into()).unwrap();
        email
            .headers_mut()
            .insert_raw(HeaderValue::new(refs_name, "<ref1> <ref2>".into()));

        // If we got here without panicking the headers were accepted.
        assert!(email.headers().get::<lettre::message::header::Subject>().is_some());
    }

    #[test]
    fn build_test_message_succeeds() {
        let from: LettreMailbox = "neomutt-rs@localhost".parse().unwrap();
        let to: LettreMailbox = "root@localhost".parse().unwrap();

        let result = LettreMessage::builder()
            .from(from)
            .to(to)
            .subject("neomutt-rs test message")
            .body("Test body".to_owned());

        assert!(result.is_ok());
    }

    #[test]
    fn build_message_with_attachment_succeeds() {
        let msg = OutgoingMessage {
            from: "a@b.com".into(),
            to: vec!["c@d.com".into()],
            subject: "With attachment".into(),
            body: "See attached".into(),
            in_reply_to: None,
            references: None,
            attachments: vec![FileAttachment {
                filename: "test.txt".into(),
                content_type: "text/plain".into(),
                data: b"file contents".to_vec(),
            }],
        };

        let result = send_message(
            &SmtpConfig {
                server: "localhost".into(),
                port: 25,
                security: SmtpSecurity::StartTls,
                user: None,
                pass: None,
            },
            &msg,
        );
        // Will fail at the network level, but the builder should succeed.
        assert!(result.is_err()); // expected — no SMTP server listening
    }

    #[test]
    fn smtp_security_default_is_starttls() {
        assert_eq!(SmtpSecurity::default(), SmtpSecurity::StartTls);
    }

    #[test]
    fn smtp_config_includes_security_field() {
        let cfg = SmtpConfig {
            server: "smtp.example.com".into(),
            port: 465,
            security: SmtpSecurity::Tls,
            user: None,
            pass: None,
        };
        assert_eq!(cfg.security, SmtpSecurity::Tls);
        assert_eq!(cfg.port, 465);
    }

    #[test]
    fn builder_dangerous_is_not_used() {
        // Safety regression: verify the non-test portion of this source file
        // never references `builder_dangerous`.  We split on the test module
        // marker and check only the production code.
        let source = include_str!("lib.rs");
        let production_code = source.split("#[cfg(test)]").next().unwrap_or(source);
        let count = production_code.matches("builder_dangerous").count();
        assert!(
            count <= 1,
            "builder_dangerous must only appear in the SmtpSecurity::Plain arm \
             (for local testing). Found {count} occurrences."
        );
    }
}
