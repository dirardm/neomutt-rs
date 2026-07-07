//! Configuration loading for neomutt-rs.
//!
//! Supports two modes:
//!
//! 1. **TOML file** — `~/.config/neomutt-rs/config.toml` (override with
//!    `NEOMUTT_CONFIG` env var).  Supports multiple `[[accounts]]` entries.
//! 2. **Env vars** — fallback when no config file is found.  Builds a
//!    single account named `"default"` from the standard `IMAP_*` /
//!    `SMTP_*` env vars.

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during configuration loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Other(String),
}

impl From<String> for ConfigError {
    fn from(s: String) -> Self {
        ConfigError::Other(s)
    }
}

impl From<&str> for ConfigError {
    fn from(s: &str) -> Self {
        ConfigError::Other(s.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// IMAP connection security mode.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ImapSecurity {
    /// Connect directly via TLS (port 993).  The default.
    #[default]
    Direct,
    /// Connect plain (port 143), then upgrade via STARTTLS.
    #[serde(alias = "starttls")]
    StartTls,
    /// Plain text, no TLS — for local testing only.
    /// **Never use in production**: credentials are sent in cleartext.
    #[serde(alias = "none")]
    Plain,
}

/// SMTP connection security mode.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum SmtpSecurity {
    /// Connect via STARTTLS (typically port 587).  The default.
    /// Plain TCP → STARTTLS command → TLS upgrade → authenticate.
    #[default]
    #[serde(alias = "starttls")]
    StartTls,
    /// Connect directly via implicit TLS (SMTPS, typically port 465).
    #[serde(alias = "tls")]
    Tls,
}

/// A single email account (IMAP + SMTP).
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub name: String,

    // IMAP
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// Connection security: `"direct"` (default) or `"starttls"`.
    #[serde(default)]
    pub imap_security: ImapSecurity,
    pub imap_user: String,
    pub imap_pass: String,
    /// OAuth2 access token for XOAUTH2 authentication.  If present,
    /// takes precedence over `imap_pass` for authentication.
    #[serde(default)]
    pub imap_oauth2_token: String,
    /// OAuth2 refresh token — used to obtain a new access token when
    /// the current one expires.  If both `imap_oauth2_token` and
    /// `imap_oauth2_refresh_token` are present, automatic refresh is
    /// attempted on auth failure.
    #[serde(default)]
    pub imap_oauth2_refresh_token: String,
    /// OAuth2 client ID (required for token refresh).
    #[serde(default)]
    pub imap_oauth2_client_id: String,
    /// OAuth2 client secret (required for token refresh with confidential
    /// clients; leave empty for public clients like some desktop apps).
    #[serde(default)]
    pub imap_oauth2_client_secret: String,
    /// OAuth2 token endpoint URL (e.g. https://oauth2.googleapis.com/token
    /// for Gmail, https://login.microsoftonline.com/common/oauth2/v2.0/token
    /// for Outlook).  Required for token refresh.
    #[serde(default)]
    pub imap_oauth2_token_endpoint: String,

    // PGP
    /// Path to a PGP secret key file (PEM-armored).  Falls back to
    /// `PGP_SIGNING_KEY` env var.
    #[serde(default)]
    pub pgp_key_path: String,
    /// Specific key ID or email to use when the keyring has multiple keys.
    #[serde(default)]
    pub pgp_key_id: String,
    /// Directory of recipient public keys for encryption (PEM-armored certs).
    #[serde(default)]
    pub pgp_keyring_dir: String,
    /// Mailbox for saving drafts.  Default: "Drafts".
    #[serde(default = "default_drafts_mailbox")]
    pub drafts_mailbox: String,

    // SMTP
    #[serde(default)]
    pub smtp_server: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Connection security: `"starttls"` (default) or `"tls"` (SMTPS).
    #[serde(default)]
    pub smtp_security: SmtpSecurity,
    #[serde(default)]
    pub smtp_user: String,
    #[serde(default)]
    pub smtp_pass: String,

    /// Outgoing From address (defaults to `imap_user` if empty).
    #[serde(default)]
    pub from: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("name", &self.name)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_security", &self.imap_security)
            .field("imap_user", &self.imap_user)
            .field("imap_pass", &"***REDACTED***")
            .field("imap_oauth2_token", &"***REDACTED***")
            .field("imap_oauth2_refresh_token", &"***REDACTED***")
            .field("imap_oauth2_client_secret", &"***REDACTED***")
            .field("pgp_key_path", &self.pgp_key_path)
            .field("pgp_key_id", &self.pgp_key_id)
            .field("pgp_keyring_dir", &self.pgp_keyring_dir)
            .field("drafts_mailbox", &self.drafts_mailbox)
            .field("smtp_server", &self.smtp_server)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_security", &self.smtp_security)
            .field("smtp_user", &self.smtp_user)
            .field("smtp_pass", &"***REDACTED***")
            .field("from", &self.from)
            .finish()
    }
}


fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    587
}

impl Account {
    /// The effective From address: explicit `from` field, else `imap_user`.
    pub fn effective_from(&self) -> &str {
        if self.from.is_empty() {
            &self.imap_user
        } else {
            &self.from
        }
    }
}

/// Notification preferences.
#[derive(Clone, Debug, Deserialize)]
pub struct NotificationConfig {
    /// Enable OS-level desktop notifications for new mail.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Show sender + subject in the notification body.  When false,
    /// only a generic "N new messages" is shown.
    #[serde(default = "default_true")]
    pub show_preview: bool,
}

/// HTML and download preferences.
#[derive(Clone, Debug, Deserialize)]
pub struct DownloadConfig {
    /// Directory for saving attachments.  Default: ~/Downloads.
    #[serde(default = "default_downloads_dir")]
    pub directory: String,
    /// Maximum attachment size in bytes.  Default: 25 MB.
    #[serde(default = "default_max_attach_size")]
    pub max_attach_size: u64,
    /// Load remote images in HTML browser view.  Default: false.
    /// When false, remote <img> tags are stripped for privacy.
    #[serde(default)]
    pub html_load_remote_images: bool,
    /// Directory for temp HTML files.  Default: system temp dir.
    #[serde(default = "default_html_temp_dir")]
    pub html_temp_dir: String,
}

fn default_html_temp_dir() -> String {
    std::env::temp_dir().join("neomutt-rs-html").to_string_lossy().to_string()
}

fn default_max_attach_size() -> u64 { 25 * 1024 * 1024 }
fn default_drafts_mailbox() -> String { "Drafts".into() }

fn default_true() -> bool { true }
fn default_downloads_dir() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/Downloads"))
        .unwrap_or_else(|_| ".".into())
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self { enabled: true, show_preview: true }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            directory: default_downloads_dir(),
            max_attach_size: default_max_attach_size(),
            html_load_remote_images: false,
            html_temp_dir: default_html_temp_dir(),
        }
    }
}

/// IMAP operational parameters (timeouts + safety limits).
#[derive(Clone, Debug, Deserialize)]
pub struct ImapTimeouts {
    /// Initial backoff delay in seconds after a connection failure.
    #[serde(default = "default_backoff_init")]
    pub backoff_init_secs: u64,
    /// Maximum backoff delay in seconds.
    #[serde(default = "default_backoff_max")]
    pub backoff_max_secs: u64,
    /// Poll interval in seconds when IDLE is not supported.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Maximum message size in bytes for body/part fetches.
    /// Messages larger than this are refused rather than buffered in full.
    /// Default: 25 MB.
    #[serde(default = "default_max_fetch_size")]
    pub max_fetch_size_bytes: u32,
    /// Maximum number of messages to cache per mailbox in SQLite.
    /// When exceeded, the oldest messages (by UID) are evicted.
    /// Default: 10 000 (enough for years of typical email volume).
    #[serde(default = "default_max_cached")]
    pub max_cached_messages_per_mailbox: usize,
    /// Maximum number of contacts to store.  When exceeded, the
    /// least-recently-seen contacts are evicted.
    /// Default: 5 000.
    #[serde(default = "default_max_contacts")]
    pub max_contacts: usize,
}

fn default_backoff_init() -> u64 { 1 }
fn default_backoff_max() -> u64 { 30 }
fn default_poll_interval() -> u64 { 30 }
fn default_max_fetch_size() -> u32 { 25 * 1024 * 1024 }
fn default_max_cached() -> usize { 10_000 }
fn default_max_contacts() -> usize { 5_000 }

impl Default for ImapTimeouts {
    fn default() -> Self {
        Self {
            backoff_init_secs: 1,
            backoff_max_secs: 30,
            poll_interval_secs: 30,
            max_fetch_size_bytes: 25 * 1024 * 1024,
            max_cached_messages_per_mailbox: 10_000,
            max_contacts: 5_000,
        }
    }
}

/// Search index parameters.
#[derive(Clone, Debug, Deserialize)]
pub struct SearchConfig {
    /// Tantivy writer buffer size in bytes.
    #[serde(default = "default_writer_buffer")]
    pub writer_buffer_bytes: usize,
    /// Maximum number of search results to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Maximum number of messages to keep in the search index, total
    /// across all accounts.  When exceeded, the oldest entries (by
    /// insertion order) are evicted, respecting account boundaries so
    /// no single account is disproportionately evicted.
    /// Default: 50 000.
    #[serde(default = "default_max_indexed")]
    pub max_indexed_messages: usize,
}

fn default_writer_buffer() -> usize { 50_000_000 }
fn default_max_results() -> usize { 50 }
fn default_max_indexed() -> usize { 50_000 }

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            writer_buffer_bytes: 50_000_000,
            max_results: 50,
            max_indexed_messages: 50_000,
        }
    }
}

/// Parsed keybinding: maps a key-combo string to a Command variant name.
pub type KeybindingMap = std::collections::HashMap<String, String>;

/// Display/layout preferences.
#[derive(Clone, Debug, Deserialize)]
pub struct DisplayConfig {
    /// Message list column widths.
    #[serde(default = "default_subject_width")]
    pub subject_width: usize,
    #[serde(default = "default_from_width")]
    pub from_width: usize,
    #[serde(default = "default_date_width")]
    pub date_width: usize,
    /// Line width for HTML-to-text conversion.
    #[serde(default = "default_text_wrap")]
    pub text_wrap_width: usize,
}

fn default_subject_width() -> usize { 40 }
fn default_from_width() -> usize { 30 }
fn default_date_width() -> usize { 24 }
fn default_text_wrap() -> usize { 80 }

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { subject_width: 40, from_width: 30, date_width: 24, text_wrap_width: 80 }
    }
}

/// Top-level TOML structure.
#[derive(Clone, Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    accounts: Vec<Account>,
    #[serde(default)]
    notifications: NotificationConfig,
    #[serde(default)]
    downloads: DownloadConfig,
    #[serde(default)]
    imap_timeouts: ImapTimeouts,
    #[serde(default)]
    search: SearchConfig,
    #[serde(default)]
    keybindings: KeybindingMap,
    #[serde(default)]
    display: DisplayConfig,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load configuration: accounts + notification + download settings.
///
/// Prefers the TOML config file; falls back to env vars for a single
/// "default" account with notifications enabled.
pub fn load_config() -> Result<(Vec<Account>, NotificationConfig, DownloadConfig, ImapTimeouts, SearchConfig, KeybindingMap, DisplayConfig), ConfigError> {
    let config_path = std::env::var("NEOMUTT_CONFIG").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.config/neomutt-rs/config.toml")
    });

    if let Ok(contents) = std::fs::read_to_string(&config_path) {
        let cfg: ConfigFile =
            toml::from_str(&contents).map_err(|e| ConfigError::Parse(format!("{config_path}: {e}")))?;
        if !cfg.accounts.is_empty() {
            return Ok((cfg.accounts, cfg.notifications, cfg.downloads, cfg.imap_timeouts, cfg.search, cfg.keybindings, cfg.display));
        }
    }

    // Fallback: single account from env vars.
    let host = std::env::var("IMAP_HOST").map_err(|_| {
        ConfigError::Other(format!(
            "no config file found at {config_path} and IMAP_HOST not set — \
             nothing to connect to"
        ))
    })?;
    let user = std::env::var("IMAP_USER").unwrap_or_default();
    let pass = std::env::var("IMAP_PASS").unwrap_or_default();
    let port = std::env::var("IMAP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(993);
    let security = std::env::var("IMAP_SECURITY")
        .ok()
        .map(|s| match s.to_lowercase().as_str() {
            "starttls" => ImapSecurity::StartTls,
            _ => ImapSecurity::Direct,
        })
        .unwrap_or_default();
    let oauth2_token = std::env::var("IMAP_OAUTH2_TOKEN").unwrap_or_default();
    let oauth2_refresh_token = std::env::var("IMAP_OAUTH2_REFRESH_TOKEN").unwrap_or_default();
    let oauth2_client_id = std::env::var("IMAP_OAUTH2_CLIENT_ID").unwrap_or_default();
    let oauth2_client_secret = std::env::var("IMAP_OAUTH2_CLIENT_SECRET").unwrap_or_default();
    let oauth2_token_endpoint = std::env::var("IMAP_OAUTH2_TOKEN_ENDPOINT").unwrap_or_default();
    let pgp_key_path = std::env::var("PGP_SIGNING_KEY").unwrap_or_default();
    let pgp_key_id = std::env::var("PGP_KEY_ID").unwrap_or_default();
    let pgp_keyring_dir = std::env::var("PGP_KEYRING_DIR").unwrap_or_default();
    let drafts_mailbox = std::env::var("DRAFTS_MAILBOX").unwrap_or_else(|_| "Drafts".into());
    let smtp_server = std::env::var("SMTP_SERVER").unwrap_or_default();
    let smtp_port = std::env::var("SMTP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(587);
    let smtp_security = std::env::var("SMTP_SECURITY")
        .ok()
        .map(|s| match s.to_lowercase().as_str() {
            "tls" => SmtpSecurity::Tls,
            _ => SmtpSecurity::StartTls,
        })
        .unwrap_or_default();
    let smtp_user = std::env::var("SMTP_USER").unwrap_or_default();
    let smtp_pass = std::env::var("SMTP_PASS").unwrap_or_default();
    let from_addr = std::env::var("SMTP_FROM").unwrap_or_default();

    let notif = NotificationConfig::default();
    let downloads = DownloadConfig::default();
    let timeouts = ImapTimeouts::default();
    let search_cfg = SearchConfig::default();
    let keybindings = KeybindingMap::new();
    let display_cfg = DisplayConfig::default();
    Ok((vec![Account {
        name: "default".into(),
        imap_host: host,
        imap_port: port,
        imap_security: security,
        imap_user: user,
        imap_pass: pass,
        imap_oauth2_token: oauth2_token,
        imap_oauth2_refresh_token: oauth2_refresh_token,
        imap_oauth2_client_id: oauth2_client_id,
        imap_oauth2_client_secret: oauth2_client_secret,
        imap_oauth2_token_endpoint: oauth2_token_endpoint,
        pgp_key_path,
        pgp_key_id,
        pgp_keyring_dir,
        drafts_mailbox,
        smtp_server,
        smtp_port,
        smtp_security,
        smtp_user,
        smtp_pass,
        from: from_addr,
    }], notif, downloads, timeouts, search_cfg, keybindings, display_cfg))
}

/// Convenience: load only the accounts list (backward compat).
pub fn load_accounts() -> Result<Vec<Account>, ConfigError> {
    load_config().map(|(accounts, _, _, _, _, _, _)| accounts)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_multi_account_toml() {
        let toml = r#"
[[accounts]]
name = "work"
imap_host = "imap.work.com"
imap_port = 993
imap_user = "me@work.com"
imap_pass = "s3cret"
smtp_server = "smtp.work.com"
smtp_port = 587
smtp_user = "me@work.com"
smtp_pass = "smtp-pass"

[[accounts]]
name = "personal"
imap_host = "imap.personal.com"
imap_user = "me@personal.com"
imap_pass = "personal123"
smtp_server = "smtp.personal.com"
"#;

        let cfg: ConfigFile = toml::from_str(toml).expect("parse TOML");
        assert_eq!(cfg.accounts.len(), 2);

        let work = &cfg.accounts[0];
        assert_eq!(work.name, "work");
        assert_eq!(work.imap_host, "imap.work.com");
        assert_eq!(work.imap_port, 993);
        assert_eq!(work.smtp_server, "smtp.work.com");
        assert_eq!(work.smtp_port, 587);
        assert_eq!(work.smtp_user, "me@work.com");
        assert_eq!(work.smtp_pass, "smtp-pass");
        assert_eq!(work.effective_from(), "me@work.com");

        let pers = &cfg.accounts[1];
        assert_eq!(pers.name, "personal");
        assert_eq!(pers.imap_port, 993); // default
        assert_eq!(pers.smtp_port, 587); // default
        assert!(pers.smtp_user.is_empty());
        assert!(pers.smtp_pass.is_empty());
        assert_eq!(pers.effective_from(), "me@personal.com");
    }

    #[test]
    fn account_with_explicit_from() {
        let toml = r#"
[[accounts]]
name = "test"
imap_host = "h"
imap_user = "user@h"
imap_pass = "p"
from = "custom@h"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.accounts[0].effective_from(), "custom@h");
    }

    #[test]
    fn default_ports_are_applied() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.imap_port, 993);
        assert_eq!(a.smtp_port, 587);
        assert!(a.smtp_server.is_empty());
        assert_eq!(a.imap_security, ImapSecurity::Direct);
        assert!(a.imap_oauth2_token.is_empty());
    }

    #[test]
    fn starttls_security_is_parsed() {
        let toml = r#"
[[accounts]]
name = "tls"
imap_host = "h"
imap_port = 143
imap_user = "u"
imap_pass = "p"
imap_security = "starttls"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.imap_security, ImapSecurity::StartTls);
        assert_eq!(a.imap_port, 143);
    }

    #[test]
    fn oauth2_token_is_parsed() {
        let toml = r#"
[[accounts]]
name = "oauth"
imap_host = "h"
imap_user = "u"
imap_pass = ""
imap_oauth2_token = "ya29.token"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.imap_oauth2_token, "ya29.token");
        assert!(a.imap_pass.is_empty());
        // New fields default to empty.
        assert!(a.imap_oauth2_refresh_token.is_empty());
        assert!(a.imap_oauth2_client_id.is_empty());
        assert!(a.imap_oauth2_client_secret.is_empty());
        assert!(a.imap_oauth2_token_endpoint.is_empty());
    }

    #[test]
    fn oauth2_refresh_fields_are_parsed() {
        let toml = r#"
[[accounts]]
name = "full"
imap_host = "h"
imap_user = "u"
imap_pass = ""
imap_oauth2_token = "access"
imap_oauth2_refresh_token = "refresh"
imap_oauth2_client_id = "cid"
imap_oauth2_client_secret = "csec"
imap_oauth2_token_endpoint = "https://example.com/token"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.imap_oauth2_token, "access");
        assert_eq!(a.imap_oauth2_refresh_token, "refresh");
        assert_eq!(a.imap_oauth2_client_id, "cid");
        assert_eq!(a.imap_oauth2_client_secret, "csec");
        assert_eq!(a.imap_oauth2_token_endpoint, "https://example.com/token");
    }

    #[test]
    fn oauth2_refresh_token_is_redacted() {
        let toml = r#"
[[accounts]]
name = "r"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
imap_oauth2_refresh_token = "secret-refresh-token"
imap_oauth2_client_secret = "secret-client"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let dbg = format!("{:?}", cfg.accounts[0]);
        assert!(!dbg.contains("secret-refresh-token"));
        assert!(!dbg.contains("secret-client"));
        assert!(dbg.contains("***REDACTED***"));
    }

    #[test]
    fn pgp_key_fields_are_parsed() {
        let toml = r#"
[[accounts]]
name = "pgp"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
pgp_key_path = "/home/user/.pgp/key.asc"
pgp_key_id = "alice@example.com"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.pgp_key_path, "/home/user/.pgp/key.asc");
        assert_eq!(a.pgp_key_id, "alice@example.com");
    }

    #[test]
    fn notification_config_defaults_to_enabled() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert!(cfg.notifications.enabled);
        assert!(cfg.notifications.show_preview);
    }

    #[test]
    fn notification_config_can_be_disabled() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"

[notifications]
enabled = false
show_preview = false
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert!(!cfg.notifications.enabled);
        assert!(!cfg.notifications.show_preview);
    }

    #[test]
    fn pgp_key_fields_default_to_empty() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert!(cfg.accounts[0].pgp_key_path.is_empty());
        assert!(cfg.accounts[0].pgp_key_id.is_empty());
    }

    #[test]
    fn imap_security_defaults_to_direct() {
        let toml = r#"
[[accounts]]
name = "d"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.accounts[0].imap_security, ImapSecurity::Direct);
    }

    // -- smtp security ----------------------------------------------------

    #[test]
    fn smtp_security_defaults_to_starttls() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.accounts[0].smtp_security, SmtpSecurity::StartTls);
        // Non-breaking upgrade: existing configs without smtp_security
        // now get StartTls (TLS-enabled) instead of the old plaintext default.
    }

    #[test]
    fn smtp_security_tls_is_parsed() {
        let toml = r#"
[[accounts]]
name = "smtps"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
smtp_server = "smtp.example.com"
smtp_security = "tls"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.smtp_security, SmtpSecurity::Tls);
    }

    #[test]
    fn smtp_security_starttls_is_parsed() {
        let toml = r#"
[[accounts]]
name = "starttls"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
smtp_server = "smtp.example.com"
smtp_security = "starttls"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let a = &cfg.accounts[0];
        assert_eq!(a.smtp_security, SmtpSecurity::StartTls);
    }

    #[test]
    fn smtp_security_appears_in_debug_output() {
        let toml = r#"
[[accounts]]
name = "s"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
smtp_security = "tls"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        let dbg = format!("{:?}", cfg.accounts[0]);
        assert!(dbg.contains("Tls"), "smtp_security should appear in Debug: {dbg}");
    }

    // -- display config defaults -------------------------------------------

    #[test]
    fn display_config_defaults_match_expected() {
        let d = DisplayConfig::default();
        assert_eq!(d.subject_width, 40);
        assert_eq!(d.from_width, 30);
        assert_eq!(d.date_width, 24);
        assert_eq!(d.text_wrap_width, 80);
    }

    #[test]
    fn display_config_parses_from_toml() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"

[display]
subject_width = 50
from_width = 35
date_width = 20
text_wrap_width = 72
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.display.subject_width, 50);
        assert_eq!(cfg.display.from_width, 35);
        assert_eq!(cfg.display.date_width, 20);
        assert_eq!(cfg.display.text_wrap_width, 72);
    }

    #[test]
    fn display_config_uses_defaults_when_not_specified() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        // All fields should be at their serde defaults when [display] is absent.
        assert_eq!(cfg.display.subject_width, 40);
        assert_eq!(cfg.display.from_width, 30);
        assert_eq!(cfg.display.date_width, 24);
        assert_eq!(cfg.display.text_wrap_width, 80);
    }

    #[test]
    fn display_config_partial_override_preserves_other_defaults() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"

[display]
subject_width = 60
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.display.subject_width, 60);
        // Other fields keep defaults.
        assert_eq!(cfg.display.from_width, 30);
        assert_eq!(cfg.display.date_width, 24);
        assert_eq!(cfg.display.text_wrap_width, 80);
    }

    // -- imap timeouts ----------------------------------------------------

    #[test]
    fn imap_timeouts_defaults_include_max_fetch_size() {
        let t = ImapTimeouts::default();
        assert_eq!(t.backoff_init_secs, 1);
        assert_eq!(t.backoff_max_secs, 30);
        assert_eq!(t.poll_interval_secs, 30);
        assert_eq!(t.max_fetch_size_bytes, 25 * 1024 * 1024);
    }

    #[test]
    fn imap_timeouts_max_fetch_size_parses_from_toml() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"

[imap_timeouts]
max_fetch_size_bytes = 10485760
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.imap_timeouts.max_fetch_size_bytes, 10 * 1024 * 1024);
        // Other fields keep defaults.
        assert_eq!(cfg.imap_timeouts.backoff_init_secs, 1);
    }

    #[test]
    fn imap_timeouts_max_cached_and_contacts_have_defaults() {
        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.imap_timeouts.max_cached_messages_per_mailbox, 10_000);
        assert_eq!(cfg.imap_timeouts.max_contacts, 5_000);
    }

    #[test]
    fn search_config_max_indexed_defaults() {
        let t = SearchConfig::default();
        assert_eq!(t.max_indexed_messages, 50_000);

        let toml = r#"
[[accounts]]
name = "minimal"
imap_host = "h"
imap_user = "u"
imap_pass = "p"

[search]
max_indexed_messages = 10000
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.search.max_indexed_messages, 10_000);
    }
}
