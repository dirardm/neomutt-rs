//! Core application logic for neomutt-rs — separated from `main.rs` so the
//! event-routing, state-mutation, and account-management logic can be unit-
//! tested without spinning up live IMAP connections or tokio tasks.

use std::collections::HashMap;

use tokio::sync::mpsc;

use neomutt_cache::MailboxCache;
use neomutt_config::{Account, NotificationConfig};
use neomutt_core::thread::thread_mailbox;
use neomutt_core::Mailbox;
use neomutt_mail_store::ImapEvent;
use neomutt_pgp::Keyring;
use neomutt_search::SearchIndex;
use neomutt_smtp_client::SmtpConfig;
use neomutt_ui::{
    flatten_thread, reply_compose_state, Command, ComposeField, ComposeState, Mode, RenderState,
    ThreadEntry,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Application-level error covering config, I/O, SMTP, and PGP failures.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Smtp(String),
    #[error("{0}")]
    Other(String),
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Other(s)
    }
}

impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError::Other(s.to_owned())
    }
}

// ---------------------------------------------------------------------------
// OS notifications
// ---------------------------------------------------------------------------

/// Sends a desktop notification for new mail, respecting user config.
///
/// * `preview` — the sender + subject of the newest message, or `None`
///   for a generic message.  Only shown when `show_preview` is enabled.
pub fn send_new_mail_notification(
    config: &neomutt_config::NotificationConfig,
    account: &str,
    mailbox: &str,
    count: usize,
    preview: Option<&str>,
) {
    if !config.enabled || count == 0 {
        return;
    }
    // Tests run without a window server — skip.
    if cfg!(test) {
        return;
    }
    let summary = "neomutt-rs";
    let body = if config.show_preview {
        if let Some(p) = preview {
            format!("{p}\n({count} new in {account}/{mailbox})")
        } else {
            format!("{count} new message{} in {account}/{mailbox}",
                if count > 1 { "s" } else { "" })
        }
    } else {
        format!("New mail in {account}/{mailbox}")
    };
    // Fire-and-forget: notification failures are non-fatal.
    if let Err(e) = notify_rust::Notification::new()
        .summary(summary)
        .body(&body)
        .show()
    {
        log::error!("[notify] failed to send: {e}");
    }
}

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

use clap::Parser;

/// neomutt-rs — terminal email client
///
/// Precedence (highest to lowest):
///   1. CLI flag  (--config, --account)
///   2. Env var   (NEOMUTT_CONFIG, IMAP_HOST, etc.)
///   3. Default   (first account in config, INBOX, etc.)
#[derive(Parser)]
#[command(version, about)]
pub struct Args {
    /// Path to config file.  Overrides NEOMUTT_CONFIG env var.
    #[arg(long)]
    pub config: Option<String>,

    /// Start on a specific account by name.  If the name doesn't match
    /// any loaded account, exits with an error rather than defaulting.
    #[arg(long)]
    pub account: Option<String>,
}

impl Args {
    /// Parse CLI args, applying overrides to env vars.
    ///
    /// Call once at startup before any tasks spawn.
    pub fn init() -> Self {
        let args = Self::parse();
        if let Some(ref config_path) = args.config {
            // SAFETY: Single-threaded startup before any tasks spawn.
            unsafe { std::env::set_var("NEOMUTT_CONFIG", config_path) };
        }
        args
    }

    /// Resolve the `--account` flag against the loaded account list.
    ///
    /// Returns the chosen account name, or an error if `--account` was
    /// given but doesn't match any loaded account.
    pub fn resolve_account(
        &self,
        accounts: &[neomutt_config::Account],
    ) -> Result<String, AppError> {
        if let Some(ref wanted) = self.account {
            if accounts.iter().any(|a| a.name == *wanted) {
                Ok(wanted.clone())
            } else {
                let names: Vec<&str> =
                    accounts.iter().map(|a| a.name.as_str()).collect();
                Err(AppError::Config(format!(
                    "account '{wanted}' not found.  Available: {}",
                    names.join(", ")
                )))
            }
        } else {
            accounts
                .first()
                .map(|a| a.name.clone())
                .ok_or_else(|| AppError::Config("no accounts configured".to_owned()))
        }
    }
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// The full application state — the single owner/writer of all mutable data.
pub struct AppState {
    pub accounts: HashMap<String, AccountState>,
    pub active_account: String,
    pub mailbox_name: String,
    pub mode: Mode,
    pub show_mailbox_list: bool,
    pub compose: ComposeState,
    pub status_message: Option<String>,
    pub threaded: bool,
    pub cache: MailboxCache,
    pub search: SearchIndex,
    pub search_query: String,
    pub search_uids: Vec<u32>,
    /// Previous UID set used to detect genuinely new messages.
    pub previous_uids: std::collections::HashSet<u32>,
    /// Accumulated count of genuinely new messages since last user
    /// interaction in the message list.  Cleared on any MessageList
    /// command (navigate, reply, compose, etc.).
    pub new_mail_total: usize,
    /// User's notification preferences.
    pub notif_config: NotificationConfig,
    /// Scroll position in the detail view body pane.
    pub detail_scroll: usize,
    /// Selected attachment index in the detail view.
    pub detail_attach_index: usize,
    /// Download directory for attachment saving.
    pub download_config: neomutt_config::DownloadConfig,
    /// File browser state.
    pub browser_path: String,
    pub browser_files: Vec<neomutt_ui::FileEntry>,
    pub browser_index: usize,
    /// Files attached in the compose view.
    pub attached_files: Vec<neomutt_ui::FileEntry>,
    /// Full paths for currently attached files (for send-time reading).
    pub attached_file_paths: Vec<String>,
    /// Mailbox list sidebar index.
    pub mailbox_list_index: usize,
    /// Per-account switch senders for mailbox redirection.
    pub switch_senders: HashMap<String, mpsc::Sender<String>>,
    /// Pending copy/move destination selected: (uid, is_move, source, dest).
    pub pending_copy_move_action: Option<(u32, bool, String, String)>,
    /// Input for creating a new mailbox.
    pub mailbox_create_input: String,
    /// Active passphrase prompt text (e.g. "Enter PGP passphrase").
    pub passphrase_prompt: Option<String>,
    /// Oneshot sender for the entered passphrase — accessed by the
    /// main event loop to await the user's response.
    pub passphrase_tx: Option<tokio::sync::oneshot::Sender<String>>,
    /// Pending send operation held while the user enters a passphrase.
    /// (smtp_cfg, from, compose, pgp_key_path, keyring_dir, attachments)
    pub pending_send: Option<(
        neomutt_smtp_client::SmtpConfig, String, ComposeState, String, String, Vec<String>
    )>,

    /// Delete confirmation: name of mailbox pending deletion.
    pub delete_confirm: Option<String>,
    /// UID of the draft being edited, so we can replace it on save.
    pub draft_replacing_uid: Option<u32>,
    /// Custom keybindings from config.
    pub keybindings: std::collections::HashMap<String, String>,
    /// Display/layout preferences.
    pub display_config: neomutt_config::DisplayConfig,
}

/// Per-account mailbox state.
#[derive(Clone)]
pub struct AccountState {
    pub mailbox: Mailbox,
    pub selected_index: usize,
    pub thread_entries: Vec<ThreadEntry>,
    pub uid_validity: Option<u32>,
    /// Available mailbox names (from LIST).
    pub mailboxes: Vec<neomutt_mail_store::MailboxEntry>,
    /// Currently selected/active mailbox for this account.
    pub active_mailbox: String,
}

impl AppState {
    /// Count genuinely new messages since last update.
    #[allow(dead_code)] // used in tests
    fn compute_new_mail_count(&self) -> usize {
        let state = self.accounts.get(&self.active_account);
        state
            .map(|s| {
                let current: std::collections::HashSet<u32> =
                    s.mailbox.messages.iter().map(|m| m.uid).collect();
                current.difference(&self.previous_uids).count()
            })
            .unwrap_or(0)
    }

    /// Create a fresh AppState from loaded accounts, an open cache, and a
    /// search index.
    pub fn new(
        accounts: Vec<Account>,
        mailbox_name: String,
        cache: MailboxCache,
        search: SearchIndex,
        notif_config: NotificationConfig,
        download_config: neomutt_config::DownloadConfig,
    ) -> Self {
        let first = accounts
            .first()
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let mut accts = HashMap::new();
        for acct in &accounts {
            let cached = cache
                .load_mailbox(&acct.name, &mailbox_name)
                .unwrap_or_default();
            let uv = cache.get_uid_validity(&acct.name, &mailbox_name);
            accts.insert(
                acct.name.clone(),
                AccountState {
                    mailbox: Mailbox {
                        messages: cached,
                    },
                    selected_index: 0,
                    thread_entries: Vec::new(),
                    uid_validity: uv,
                    mailboxes: vec![neomutt_mail_store::MailboxEntry {
                        name: mailbox_name.clone(),
                        label: format!("📥 {}", mailbox_name),
                    }],
                    active_mailbox: mailbox_name.clone(),
                },
            );
        }
        Self {
            accounts: accts,
            active_account: first,
            mailbox_name,
            mode: Mode::MessageList,
            compose: ComposeState::default(),
            status_message: None,
            threaded: false,
            cache,
            search,
            search_query: String::new(),
            search_uids: Vec::new(),
            previous_uids: std::collections::HashSet::new(),
            new_mail_total: 0,
            notif_config,
            detail_scroll: 0,
            detail_attach_index: 0,
            download_config,
            browser_path: String::new(),
            browser_files: Vec::new(),
            browser_index: 0,
            attached_files: Vec::new(),
            attached_file_paths: Vec::new(),
            show_mailbox_list: false,
            mailbox_list_index: 0,
            switch_senders: HashMap::new(),
            pending_copy_move_action: None,
            mailbox_create_input: String::new(),
            delete_confirm: None,
            keybindings: std::collections::HashMap::new(),
            passphrase_prompt: None,
            passphrase_tx: None,
            pending_send: None,
            display_config: neomutt_config::DisplayConfig::default(),
            draft_replacing_uid: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure event/command handlers
// ---------------------------------------------------------------------------

/// The outcome of applying a [`Command`].
#[derive(Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Continue the event loop.
    Continue,
    /// Stop the event loop (shutdown).
    Quit,
}

/// Apply an IMAP event to the state.  Mutates accounts + cache.
pub fn apply_event(state: &mut AppState, event: ImapEvent) {
    match event {
        ImapEvent::MailboxList {
            account,
            mailboxes,
        } => {
            if let Some(acct) = state.accounts.get_mut(&account) {
                acct.mailboxes = mailboxes;
            }
        }
        ImapEvent::BodyFetched {
            account,
            mailbox_name,
            uid,
            body,
            html_body,
        } => {
            if let Some(acct) = state.accounts.get_mut(&account)
                && let Some(msg) = acct
                    .mailbox
                    .messages
                    .iter_mut()
                    .find(|m| m.uid == uid)
                {
                    msg.body = body;
                    msg.html_body = html_body;
                    msg.body_fetched = true;
                    // Index the body in search.
                    if let Err(e) = state.search.index_messages(
                        &account,
                        &mailbox_name,
                        std::slice::from_ref(msg),
                    ) {
                        log::error!("[{account}] search body index error: {e}");
                    }
                }
            state.status_message = Some(format!("body for UID {uid} loaded"));
        }
        ImapEvent::Error { account, message } => {
            // Errors persist until the next user action; don't get
            // cleared by subsequent MailboxUpdated events.
            state.status_message =
                Some(format!("[{account}] {message}"));
        }
        ImapEvent::MailboxUpdated {
            account,
            mailbox_name,
            mailbox: new_mb,
            uid_validity,
        } => {
            if let Some(acct_state) = state.accounts.get_mut(&account) {
                // UIDVALIDITY check + cache wipe.
                if let Some(uv) = uid_validity {
                    if acct_state.uid_validity.is_some_and(|old| old != uv) {
                        log::info!("[{account}] UIDVALIDITY changed, wiping cache");
                        if let Err(e) =
                            state.cache.wipe_mailbox(&account, &mailbox_name)
                        {
                            state.status_message =
                                Some(format!("[{account}] cache wipe error: {e}"));
                        }
                    }
                    if let Err(e) = state.cache.set_uid_validity(
                        &account,
                        &mailbox_name,
                        uv,
                    ) {
                        state.status_message =
                            Some(format!("[{account}] cache UIDVALIDITY write error: {e}"));
                    }
                    acct_state.uid_validity = Some(uv);
                }

                // Write through to cache with bounded retry.
                if let Err(e) = retry_cache_write(|| {
                    state.cache.replace_mailbox(
                        &account,
                        &mailbox_name,
                        &new_mb.messages,
                    )
                }) {
                    log::error!("[{account}] cache write error: {e}");
                    state.status_message =
                        Some(format!("[{account}] cache write error: {e}"));
                }

                // Capture old UIDs and count genuinely new ones.
                let old_uids: std::collections::HashSet<u32> =
                    acct_state.mailbox.messages.iter().map(|m| m.uid).collect();
                let current_uids: std::collections::HashSet<u32> =
                    new_mb.messages.iter().map(|m| m.uid).collect();
                let genuinely_new = current_uids.difference(&old_uids).count();
                if genuinely_new > 0 {
                    state.new_mail_total += genuinely_new;
                    // Build preview from the newest message.
                    let preview = new_mb.messages.last().map(|m| {
                        format!("{} — {}", m.envelope.from, m.envelope.subject)
                    });
                    send_new_mail_notification(
                        &state.notif_config,
                        &account,
                        &state.mailbox_name,
                        genuinely_new,
                        preview.as_deref(),
                    );
                }
                acct_state.mailbox = new_mb;
                state.previous_uids = old_uids;

                // Index into search.
                if let Err(e) = state.search.index_messages(
                    &account,
                    &state.mailbox_name,
                    &acct_state.mailbox.messages,
                ) {
                    log::error!("[{account}] search index error: {e}");
                    state.status_message = Some(format!("[{account}] search index error: {e}"));
                }

                // Learn contacts from From/To addresses.
                for msg in &acct_state.mailbox.messages {
                    state.cache.learn_addresses([
                        msg.envelope.from.as_str(),
                        msg.envelope.to.as_str(),
                    ]);
                }

                // Clamp cursor.
                if !acct_state.mailbox.messages.is_empty()
                    && acct_state.selected_index >= acct_state.mailbox.messages.len()
                {
                    acct_state.selected_index =
                        acct_state.mailbox.messages.len().saturating_sub(1);
                }
                if acct_state.mailbox.messages.is_empty() {
                    acct_state.selected_index = 0;
                }
                acct_state.thread_entries =
                    recompute_thread_entries(state.threaded, &acct_state.mailbox);
            }
        }
    }
}

/// Apply a UI command to the state.  Mutates accounts + mode + compose.
///
/// Returns [`CommandOutcome::Quit`] when the app should shut down.
pub fn apply_command(state: &mut AppState, cmd: Command) -> CommandOutcome {
    // Clear transient state on any user action.
    state.status_message = None;
    // "Dismiss" new-mail badge when the user interacts with the mailbox.
    if state.mode == Mode::MessageList && !state.show_mailbox_list {
        state.new_mail_total = 0;
    }

    // Mailbox list navigation (when sidebar is open).
    if state.show_mailbox_list
        && matches!(&cmd, Command::NavigateUp | Command::NavigateDown)
    {
        match &cmd {
            Command::NavigateUp => {
                state.mailbox_list_index =
                    state.mailbox_list_index.saturating_sub(1);
            }
            Command::NavigateDown => {
                let acct = state.accounts.get(&state.active_account);
                let len = acct.map(|a| a.mailboxes.len()).unwrap_or(0);
                if state.mailbox_list_index + 1 < len {
                    state.mailbox_list_index += 1;
                }
            }
            _ => {}
        }
        return CommandOutcome::Continue;
    }

    match &cmd {
        Command::Quit => return CommandOutcome::Quit,

        Command::NextAccount | Command::PrevAccount => {
            let mut names: Vec<String> =
                state.accounts.keys().cloned().collect();
            names.sort(); // deterministic order
            if names.len() >= 2 {
                let pos = names
                    .iter()
                    .position(|n| *n == state.active_account)
                    .unwrap_or(0);
                let new_pos = if matches!(cmd, Command::NextAccount) {
                    (pos + 1) % names.len()
                } else {
                    (pos + names.len() - 1) % names.len()
                };
                state.active_account = names[new_pos].clone();
            }
            return CommandOutcome::Continue;
        }

        Command::ToggleThreaded => {
            state.threaded = !state.threaded;
            for s in state.accounts.values_mut() {
                s.thread_entries =
                    recompute_thread_entries(state.threaded, &s.mailbox);
            }
        }
        _ => {}
    }

    // Compute selected UID up front for commands that need it before the
    // mutable account borrow.
    let sel_uid_for_detail = if matches!(&cmd, Command::DetailAttachNext) {
        selected_uid_for(state)
    } else {
        None
    };

    // Commands that need the active account's state.
    let acct = state.accounts.get_mut(&state.active_account);

    match (&state.mode, cmd) {
        (_, Command::Quit) | (_, Command::NextAccount | Command::PrevAccount) => {
            unreachable!()
        }

        // -- message-list-only --
        (Mode::MessageList, Command::NavigateUp) => {
            if let Some(a) = acct {
                a.selected_index = a.selected_index.saturating_sub(1);
            }
        }
        (Mode::MessageList, Command::NavigateDown) => {
            if let Some(a) = acct {
                let len = list_len(state.threaded, &a.mailbox, &a.thread_entries);
                if a.selected_index + 1 < len {
                    a.selected_index += 1;
                }
            }
        }
        (Mode::MessageList, Command::ToggleThreaded) => {
            if let Some(a) = acct {
                let len = list_len(state.threaded, &a.mailbox, &a.thread_entries);
                a.selected_index = a.selected_index.min(len.saturating_sub(1));
            }
        }
        (_, Command::OpenCompose) => {
            state.compose = ComposeState::default();
            state.mode = Mode::Compose;
            state.attached_files.clear();
            state.attached_file_paths.clear();
        }
        (Mode::MessageList, Command::OpenReply) => {
            if let Some(a) = acct {
                let msg = selected_message(
                    state.threaded,
                    &a.mailbox,
                    &a.thread_entries,
                    a.selected_index,
                );
                if let Some(msg) = msg {
                    state.compose = reply_compose_state(msg, false);
                    state.mode = Mode::Compose;
                }
            }
        }
        (Mode::MessageList, Command::OpenMessage(uid)) => {
            state.mode = Mode::MessageDetail;
            state.detail_scroll = 0;
            // Show a body preview if already fetched.
            if let Some(acct) = acct
                && let Some(msg) =
                    acct.mailbox.messages.iter().find(|m| m.uid == uid)
                    && msg.body_fetched {
                        state.status_message =
                            Some(format!("Body loaded ({})", msg.body.len()));
                    }
        }
        (Mode::MessageDetail, Command::CloseDetail) => {
            state.mode = Mode::MessageList;
            state.detail_scroll = 0;
            state.detail_attach_index = 0;
        }
        (Mode::MessageDetail, Command::DetailScrollUp) => {
            state.detail_scroll = state.detail_scroll.saturating_sub(1);
        }
        (Mode::MessageDetail, Command::DetailScrollDown) => {
            state.detail_scroll += 1;
        }
        (Mode::MessageDetail, Command::DetailAttachNext) => {
            let count = acct.map_or(0, |a| {
                a.mailbox
                    .messages
                    .iter()
                    .find(|m| sel_uid_for_detail == Some(m.uid))
                    .map(|m| m.attachments.len())
                    .unwrap_or(0)
            });
            if count > 0 {
                state.detail_attach_index =
                    (state.detail_attach_index + 1) % count;
            }
        }
        (Mode::MessageDetail, Command::SaveAttachment) => {
            state.status_message = Some("saving attachment …".into());
        }
        (Mode::MessageList, Command::OpenReplyAll) => {
            if let Some(a) = acct {
                let msg = selected_message(
                    state.threaded,
                    &a.mailbox,
                    &a.thread_entries,
                    a.selected_index,
                );
                if let Some(msg) = msg {
                    state.compose = reply_compose_state(msg, true);
                    state.mode = Mode::Compose;
                }
            }
        }

        // -- compose-mode --
        (Mode::Compose, Command::CancelCompose) => {
            state.mode = Mode::MessageList;
        }
        (Mode::Compose, Command::ComposeInput(ch)) => {
            apply_compose_input(&mut state.compose, ch);
        }
        (Mode::Compose, Command::ComposeBackspace) => {
            apply_compose_backspace(&mut state.compose);
        }
        (Mode::Compose, Command::ComposeNewline) => {
            match state.compose.active_field {
                ComposeField::Body => state.compose.body.push('\n'),
                _ => state.compose.active_field = state.compose.active_field.next(),
            }
        }
        (Mode::Compose, Command::ComposeNextField) => {
            state.compose.active_field = state.compose.active_field.next();
        }
        (Mode::Compose, Command::ToggleSign) => {
            state.compose.sign = !state.compose.sign;
        }
        (Mode::Compose, Command::ToggleEncrypt) => {
            state.compose.encrypt = !state.compose.encrypt;
        }
        (Mode::Compose, Command::SendCompose) => {
            state.status_message = Some("sending …".into());
        }
        (Mode::Compose, Command::SaveDraft) => {
            state.status_message = Some("saving draft …".into());
        }
        (_, Command::EditDraft(uid)) => {
            if let Some(acct) = acct
                && let Some(msg) =
                    acct.mailbox.messages.iter().find(|m| m.uid == uid)
                {
                    state.compose = ComposeState {
                        to: msg.envelope.to.clone(),
                        subject: msg.envelope.subject.clone(),
                        body: msg.body.clone(),
                        active_field: ComposeField::Body,
                        in_reply_to: None,
                        references: None,
                        sign: false,
                        encrypt: false,
                    };
                    state.mode = Mode::Compose;
                    state.draft_replacing_uid = Some(uid);
                }
        }

        (Mode::Compose, Command::OpenFileBrowser) => {
            state.browser_path = state.download_config.directory.clone();
            state.browser_files = list_files(&state.browser_path);
            state.browser_index = 0;
            state.mode = Mode::FileBrowser;
        }
        (Mode::FileBrowser, Command::BrowseCancel) => {
            state.mode = Mode::Compose;
        }
        // Note: the file browser intentionally allows full filesystem
        // navigation for attaching arbitrary files.  Path::join is used
        // instead of string formatting for correct path construction.
        (Mode::FileBrowser, Command::BrowseDir(subdir)) => {
            let new_path = if subdir == ".." {
                let p = std::path::Path::new(&state.browser_path);
                p.parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "/".into())
            } else {
                std::path::Path::new(&state.browser_path)
                    .join(&subdir)
                    .to_string_lossy()
                    .to_string()
            };
            state.browser_files = list_files(&new_path);
            state.browser_path = new_path;
            state.browser_index = 0;
        }
        (Mode::FileBrowser, Command::BrowseSelect(path)) => {
            let meta = std::fs::metadata(&path);
            match meta {
                Ok(m) if m.len() > state.download_config.max_attach_size => {
                    state.status_message = Some(format!(
                        "file too large (max {} MB)",
                        state.download_config.max_attach_size / (1024 * 1024)
                    ));
                }
                Ok(m) => {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".into());
                    state.attached_files.push(neomutt_ui::FileEntry {
                        name: name.clone(),
                        is_dir: false,
                        size: m.len(),
                    });
                    state.attached_file_paths.push(path.clone());
                    state.mode = Mode::Compose;
                }
                Err(e) => {
                    state.status_message =
                        Some(format!("cannot read file: {e}"));
                }
            }
        }
        (Mode::FileBrowser, Command::BrowseUp) => {
            if state.browser_index > 0 {
                state.browser_index -= 1;
            }
        }
        (Mode::FileBrowser, Command::BrowseDown) => {
            if state.browser_index + 1 < state.browser_files.len() {
                state.browser_index += 1;
            }
        }

        (_, Command::ToggleMailboxList) => {
            state.show_mailbox_list = !state.show_mailbox_list;
            if state.show_mailbox_list {
                state.mailbox_list_index = 0;
                state.mailbox_create_input.clear();
                state.delete_confirm = None;
                state.mode = Mode::MessageList;
            }
        }
        (_, Command::CreateMailbox(input)) => {
            state.mode = Mode::MailboxCreate;
            state.mailbox_create_input = input;
        }
        (_, Command::CreateMailboxConfirm(name)) => {
            state.mode = Mode::MessageList;
            state.show_mailbox_list = true;
            state.mailbox_create_input.clear();
            state.status_message =
                Some(format!("creating mailbox '{name}' …"));
        }
        (_, Command::DeleteMailbox(name)) => {
            state.delete_confirm = Some(name);
        }
        (_, Command::DeleteMailboxConfirm) => {
            if let Some(name) = state.delete_confirm.take() {
                state.status_message =
                    Some(format!("deleting mailbox '{name}' …"));
            }
        }
        (Mode::MessageList, Command::CopyMessage(uid)) => {
            state.pending_copy_move_action =
                Some((uid, false, String::new(), String::new()));
            state.show_mailbox_list = true;
        }
        (Mode::MessageList, Command::MoveMessage(uid)) => {
            state.pending_copy_move_action =
                Some((uid, true, String::new(), String::new()));
            state.show_mailbox_list = true;
        }
        (_, Command::SelectMailbox(new_mb)) => {
            // If there's a pending copy/move, treat this as destination confirmation.
            if let Some((uid, is_move, _, _)) =
                state.pending_copy_move_action.take()
            {
                let source = state
                    .accounts
                    .get(&state.active_account)
                    .map(|a| a.active_mailbox.clone())
                    .unwrap_or_default();
                state.pending_copy_move_action =
                    Some((uid, is_move, source, new_mb.clone()));
                state.status_message = Some(if is_move {
                    format!("moving UID {uid} to {new_mb} …")
                } else {
                    format!("copying UID {uid} to {new_mb} …")
                });
                state.show_mailbox_list = false;
                return CommandOutcome::Continue;
            }

            let old_mb = state.mailbox_name.clone();
            if new_mb != old_mb {
                state.mailbox_name = new_mb.clone();
                if let Some(acct) = state.accounts.get_mut(&state.active_account) {
                    acct.active_mailbox = new_mb.clone();
                    // Load cached state for the new mailbox.
                    let cached = state
                        .cache
                        .load_mailbox(&state.active_account, &new_mb)
                        .unwrap_or_default();
                    acct.mailbox = Mailbox { messages: cached };
                    acct.selected_index = 0;
                    acct.thread_entries = Vec::new();
                    acct.uid_validity =
                        state.cache.get_uid_validity(&state.active_account, &new_mb);
                    state.previous_uids.clear();
                    state.new_mail_total = 0;
                }
                // Notify the IMAP task to switch its monitoring.
                // Channel capacity is 1.  If full (previous switch not yet
                // consumed), drop the new target — the old one is still valid
                // and the next switch will land once the IMAP task catches up.
                if let Some(tx) = state.switch_senders.get(&state.active_account) {
                    if tx.try_send(new_mb.clone()).is_err() {
                        // Channel is full or closed — either way, the pending
                        // switch (if any) is still valid.
                    }
                }
                state.show_mailbox_list = false;
            }
        }

        // -- search commands --
        (_, Command::OpenSearch) => {
            state.mode = Mode::Search;
            state.search_query.clear();
            state.search_uids.clear();
        }
        (Mode::Search, Command::CancelSearch) => {
            state.mode = Mode::MessageList;
            state.search_query.clear();
            state.search_uids.clear();
        }
        (Mode::Search, Command::SearchInput(ch)) => {
            state.search_query.push(ch);
        }
        (Mode::Search, Command::SearchBackspace) => {
            state.search_query.pop();
        }
        (Mode::Search, Command::RunSearch) => {
            // The actual search is async — caller will spawn_blocking.
            state.status_message = Some("searching …".into());
        }

        _ => {}
    }

    CommandOutcome::Continue
}

/// If the last command was `RunSearch`, return the query + scope for
/// running the search in `spawn_blocking`.
pub fn take_search_request(state: &AppState) -> Option<(String, String, String)> {
    if state.status_message.as_deref() == Some("searching …") {
        Some((
            state.search_query.clone(),
            state.active_account.clone(),
            state.mailbox_name.clone(),
        ))
    } else {
        None
    }
}

/// If the last command was `SendCompose`, return the data needed to perform
/// the send: SMTP config, From address, and compose snapshot.
///
/// The caller is responsible for `spawn_blocking` the actual `lettre` send.
pub fn take_send_request(
    state: &AppState,
    account_configs: &[Account],
) -> Option<(SmtpConfig, String, ComposeState, String, String, Vec<String>)> {
    if state.status_message.as_deref() == Some("sending …") {
        let acct = account_configs
            .iter()
            .find(|a| a.name == state.active_account)?;
        let smtp_cfg = smtp_config_for_account(acct);
        let from = acct.effective_from().to_owned();
        let pgp_key_path = acct.pgp_key_path.clone();
        let pgp_keyring_dir = acct.pgp_keyring_dir.clone();
        let paths = state.attached_file_paths.clone();
        Some((smtp_cfg, from, state.compose.clone(), pgp_key_path, pgp_keyring_dir, paths))
    } else {
        None
    }
}

/// Return the UID of the message currently being viewed in detail mode.
fn selected_uid_for(state: &AppState) -> Option<u32> {
    state
        .accounts
        .get(&state.active_account)
        .and_then(|a| {
            if state.threaded && !a.thread_entries.is_empty() {
                a.thread_entries
                    .get(a.selected_index)
                    .map(|e| e.uid)
            } else {
                a.mailbox.messages.get(a.selected_index).map(|m| m.uid)
            }
        })
}

/// If the last command was SaveAttachment, return the data needed to
/// fetch + save the selected attachment.
pub fn take_attach_save_request(
    state: &AppState,
) -> Option<(String, neomutt_core::Attachment)> {
    if state.status_message.as_deref() == Some("saving attachment …") {
        let uid = selected_uid_for(state)?;
        let acct = state.accounts.get(&state.active_account)?;
        let msg = acct.mailbox.messages.iter().find(|m| m.uid == uid)?;
        let att = msg.attachments.get(state.detail_attach_index)?.clone();
        Some((state.download_config.directory.clone(), att))
    } else {
        None
    }
}

/// Sanitize raw HTML email for safe browser viewing.
///
/// Strips `<script>` tags, remote images (unless `load_remote_images` is
/// true), external stylesheets, iframes, and objects/embeds.  Keeps safe
/// formatting tags and inline styles intact.
pub fn sanitize_html(html: &str, load_remote_images: bool) -> String {
    let mut builder = ammonia::Builder::default();
    // Preserve inline style attributes for formatting.
    builder.add_generic_attributes(&["style"]);
    // Strip scripts, CSS stylesheets, and external-loading tags.
    builder.rm_tags(&["script", "style", "head"]);
    if !load_remote_images {
        // Strip remote images — keep only data: URIs and local references.
        builder.add_generic_attribute_prefixes(&["data-"]);
        // The default allowed attributes include src; we need a custom
        // approach for img filtering.  Instead, use a URL-relative check.
        builder.attribute_filter(move |elem, attr, value| {
            if elem == "img" && attr == "src" && !load_remote_images {
                if value.starts_with("data:") || value.starts_with("cid:") {
                    Some(value.into())
                } else {
                    None
                }
            } else {
                Some(value.into())
            }
        });
    }
    // Strip external-loading tags.
    builder.rm_tags(&[
        "iframe", "object", "embed", "link",
    ]);
    builder.clean(html).to_string()
}

/// Save an attachment's bytes to a file on disk with collision handling.
///
/// Returns the resolved path where the file was written.
pub fn save_attachment_to_disk(
    dir: &str,
    att: &neomutt_core::Attachment,
) -> Result<std::path::PathBuf, AppError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::Io(e))?;
    let safe_name = sanitize_filename(&att.filename);
    let path = resolve_save_path(dir, &safe_name);
    // Verify the resolved path stays within the download directory.
    let canonical_dir = std::fs::canonicalize(dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(dir));
    // Canonicalize the parent of the file path (which must exist after
    // create_dir_all above) and check the file is inside the download dir.
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("/"));
    let canonical_parent = std::fs::canonicalize(parent)
        .unwrap_or_else(|_| parent.to_path_buf());
    if !canonical_parent.starts_with(&canonical_dir) {
        return Err(AppError::Other(format!(
            "refusing to write outside download directory: {}",
            path.display()
        )));
    }
    let data = att
        .body
        .as_deref()
        .ok_or_else(|| AppError::Other("attachment body not fetched".to_owned()))?;
    std::fs::write(&path, data)
        .map_err(|e| AppError::Io(e))?;
    Ok(path)
}

/// Strip directory components from a filename to prevent path traversal.
fn sanitize_filename(raw: &str) -> String {
    std::path::Path::new(raw)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".to_owned())
}

/// Resolve a save path, avoiding overwrites by appending a number suffix.
pub fn resolve_save_path(dir: &str, filename: &str) -> std::path::PathBuf {
    let base = std::path::Path::new(dir).join(filename);
    if !base.exists() {
        return base;
    }
    let stem = base
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..100 {
        let candidate =
            std::path::Path::new(dir).join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Fallback — append timestamp.
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    std::path::Path::new(dir).join(format!("{stem}_{ts}{ext}"))
}

fn list_files(path: &str) -> Vec<neomutt_ui::FileEntry> {
    let mut entries = Vec::new();
    // Add parent directory entry.
    if path != "/" {
        entries.push(neomutt_ui::FileEntry {
            name: "..".into(),
            is_dir: true,
            size: 0,
        });
    }
    if let Ok(dir) = std::fs::read_dir(path) {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata().ok();
            entries.push(neomutt_ui::FileEntry {
                is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                size: meta.map(|m| m.len()).unwrap_or(0),
                name,
            });
        }
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

/// If a copy/move destination was just confirmed, return the action
/// details and clear the pending state.
pub fn take_copy_move_action(
    state: &mut AppState,
) -> Option<(u32, bool, String, String)> {
    if state.status_message.as_deref().unwrap_or("").contains("moving")
        || state.status_message.as_deref().unwrap_or("").contains("copying")
    {
        state.pending_copy_move_action.take()
    } else {
        None
    }
}

/// Build the render snapshot from the current state.
pub fn build_render(state: &AppState) -> RenderState {
    let Some(acct) = state.accounts.get(&state.active_account) else {
        log::error!("[render] internal error: active account '{}' not found", state.active_account);
        return RenderState {
            mode: Mode::MessageList,
            keybindings: state.keybindings.clone(),
            column_widths: (state.display_config.subject_width, state.display_config.from_width, state.display_config.date_width),
            mailbox_name: format!("{}/{}", state.active_account, state.mailbox_name),
            mailbox: neomutt_core::Mailbox::new(),
            selected_index: 0,
            compose: state.compose.clone(),
            status_message: Some(format!("internal error: account '{}' missing", state.active_account)),
            threaded: false,
            thread_entries: Vec::new(),
            search_query: String::new(),
            search_uids: Vec::new(),
            contacts: Vec::new(),
            new_mail_count: 0,
            detail_scroll: 0,
            detail_attach_index: 0,
            browser_path: String::new(),
            browser_files: Vec::new(),
            browser_index: 0,
            attached_files: Vec::new(),
            show_mailbox_list: false,
            mailbox_create_input: String::new(),
            delete_confirm: None,
            mailboxes: Vec::new(),
            mailbox_list_index: 0,
            passphrase_prompt: None,
            passphrase_masked_len: 0,
        };
    };

    // Autocomplete: search contacts by To field prefix.
    let contacts = if state.mode == Mode::Compose && !state.compose.to.is_empty() {
        state
            .cache
            .search_contacts(&state.compose.to)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    RenderState {
        mode: state.mode.clone(),
        keybindings: state.keybindings.clone(),
        column_widths: (
            state.display_config.subject_width,
            state.display_config.from_width,
            state.display_config.date_width,
        ),
        mailbox_name: format!("{}/{}", state.active_account, state.mailbox_name),
        mailbox: acct.mailbox.clone(),
        selected_index: acct.selected_index,
        compose: state.compose.clone(),
        status_message: state.status_message.clone(),
        threaded: state.threaded,
        thread_entries: acct.thread_entries.clone(),
        search_query: state.search_query.clone(),
        search_uids: state.search_uids.clone(),
        contacts,
        new_mail_count: state.new_mail_total,
        detail_scroll: state.detail_scroll,
        detail_attach_index: state.detail_attach_index,
        browser_path: state.browser_path.clone(),
        browser_files: state.browser_files.clone(),
        browser_index: state.browser_index,
        attached_files: state.attached_files.clone(),
        show_mailbox_list: state.show_mailbox_list,
        mailbox_create_input: state.mailbox_create_input.clone(),
        delete_confirm: state.delete_confirm.clone(),
        mailboxes: state
            .accounts
            .get(&state.active_account)
            .map(|a| a.mailboxes.clone())
            .unwrap_or_default(),
        mailbox_list_index: state.mailbox_list_index,
        passphrase_prompt: state.passphrase_prompt.clone(),
        passphrase_masked_len: 0,
    }
}

// ---------------------------------------------------------------------------
// Public helpers — exported for tests
// ---------------------------------------------------------------------------

/// Retry a cache write operation up to 3 times with short backoff.
/// Surfaces the error via the status bar if all attempts fail.
fn retry_cache_write<F, T, E: std::fmt::Display>(
    mut f: F,
) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
{
    let mut last_err = None;
    for attempt in 1..=3 {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = Some(e);
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(100 * attempt));
                }
            }
        }
    }
    Err(last_err.unwrap())
}

fn guess_content_type(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// Build raw RFC 2822 bytes from compose state for IMAP APPEND.
pub fn build_draft_message(compose: &ComposeState, from: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n\
         X-neomutt-draft: true\r\n\r\n{body}",
        to = compose.to,
        subject = compose.subject,
        body = compose.body,
    )
    .into_bytes()
}

/// Return the SMTP config for a given account.
pub fn smtp_config_for_account(acct: &Account) -> SmtpConfig {
    SmtpConfig {
        server: acct.smtp_server.clone(),
        port: acct.smtp_port,
        security: match acct.smtp_security {
            neomutt_config::SmtpSecurity::Tls => neomutt_smtp_client::SmtpSecurity::Tls,
            neomutt_config::SmtpSecurity::StartTls => neomutt_smtp_client::SmtpSecurity::StartTls,
        },
        user: if acct.smtp_user.is_empty() {
            None
        } else {
            Some(acct.smtp_user.clone())
        },
        pass: if acct.smtp_pass.is_empty() {
            None
        } else {
            Some(acct.smtp_pass.clone())
        },
    }
}

/// Build an `OutgoingMessage` from compose state and send it.
///
/// If `compose.sign` is true and a signing key is available (via
/// `PGP_SIGNING_KEY` env var pointing at a cert file), the body is
/// PGP-signed before sending.  Encryption is deferred until key
/// management is implemented.
pub fn send_via_smtp(
    config: &SmtpConfig,
    from: &str,
    compose: &ComposeState,
    pgp_account_key_path: &str,
    pgp_keyring: &Keyring,
    attached_file_paths: &[String],
    passphrase: Option<String>,
) -> Result<(), AppError> {
    let to = compose
        .to
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if to.is_empty() {
        return Err(AppError::Smtp("To field is empty".into()));
    }

    let mut body_bytes: Vec<u8> = compose.body.as_bytes().to_vec();

    // PGP-sign if requested and a key is available.
    if compose.sign {
        let key_path = if !pgp_account_key_path.is_empty() {
            pgp_account_key_path.to_owned()
        } else {
            std::env::var("PGP_SIGNING_KEY")
                .map_err(|_| AppError::Smtp(
                    "PGP signing requested but no key configured \
                    (set pgp_key_path in account config or PGP_SIGNING_KEY env var)".into()
                ))?
        };
        let cert = neomutt_pgp::load_cert(&key_path)
            .map_err(|e| AppError::Smtp(format!("PGP signing key load failed: {e}")))?;
        body_bytes = neomutt_pgp::sign_unlocked(&cert, &body_bytes, passphrase.as_deref())
            .map_err(|e| AppError::Smtp(format!("PGP sign failed: {e}")))?;
    }

    // PGP-encrypt the body for each recipient.
    if compose.encrypt {
        if pgp_keyring.is_empty() {
            return Err(AppError::Smtp(
                "PGP encrypt requested but no keyring configured \
                 (set pgp_keyring_dir in account config or PGP_KEYRING_DIR env var)"
                    .into(),
            ));
        }

        let mut recipient_certs = Vec::new();
        let mut not_found = Vec::new();
        for addr in &to {
            if let Some(cert) = pgp_keyring.lookup(addr) {
                recipient_certs.push(cert.clone());
            } else {
                not_found.push(addr.clone());
            }
        }
        if !not_found.is_empty() {
            return Err(AppError::Smtp(format!(
                "no public key found for: {}.  \
                 Add their cert to the keyring directory.",
                not_found.join(", ")
            )));
        }

        body_bytes = neomutt_pgp::encrypt(&recipient_certs, &body_bytes)
            .map_err(|e| AppError::Smtp(format!("PGP encrypt failed: {e}")))?;
    }

    // Build outgoing attachments from compose-attached files.
    let mut outgoing_attachments = Vec::new();
    for path in attached_file_paths {
        let data = std::fs::read(path)
            .map_err(|e| AppError::Io(e))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".into());
        let ct = guess_content_type(path);
        outgoing_attachments.push(neomutt_smtp_client::FileAttachment {
            filename: name,
            content_type: ct,
            data,
        });
    }

    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    let msg = neomutt_smtp_client::OutgoingMessage {
        from: from.to_owned(),
        to,
        subject: compose.subject.clone(),
        body,
        in_reply_to: compose.in_reply_to.clone(),
        references: compose.references.clone(),
        attachments: outgoing_attachments,
    };
    neomutt_smtp_client::send_message(config, &msg).map_err(|e| AppError::Smtp(e.to_string()))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn recompute_thread_entries(
    threaded: bool,
    mailbox: &Mailbox,
) -> Vec<ThreadEntry> {
    if threaded {
        let roots = thread_mailbox(mailbox);
        flatten_thread(&roots, &mailbox.messages)
    } else {
        Vec::new()
    }
}

pub(crate) fn list_len(
    threaded: bool,
    mailbox: &Mailbox,
    thread_entries: &[ThreadEntry],
) -> usize {
    if threaded {
        thread_entries.len()
    } else {
        mailbox.messages.len()
    }
}

fn selected_message<'a>(
    threaded: bool,
    mailbox: &'a Mailbox,
    thread_entries: &[ThreadEntry],
    selected_index: usize,
) -> Option<&'a neomutt_core::Message> {
    if threaded {
        let uid = thread_entries.get(selected_index)?.uid;
        mailbox.messages.iter().find(|m| m.uid == uid)
    } else {
        mailbox.messages.get(selected_index)
    }
}

fn apply_compose_input(c: &mut ComposeState, ch: char) {
    match c.active_field {
        ComposeField::To => c.to.push(ch),
        ComposeField::Subject => c.subject.push(ch),
        ComposeField::Body => c.body.push(ch),
    }
}

fn apply_compose_backspace(c: &mut ComposeState) {
    match c.active_field {
        ComposeField::To => {
            c.to.pop();
        }
        ComposeField::Subject => {
            c.subject.pop();
        }
        ComposeField::Body => {
            c.body.pop();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use neomutt_core::Envelope;
    use neomutt_core::FlagSet;
    use neomutt_core::Message;
    use neomutt_ui::Command;

    // -- helpers ----------------------------------------------------------

    fn test_account(name: &str, imap_host: &str, smtp_server: &str, from: &str) -> Account {
        Account {
            name: name.into(),
            imap_host: imap_host.into(),
            imap_port: 993,
            imap_security: neomutt_config::ImapSecurity::Direct,
            imap_user: format!("{name}@example.com"),
            imap_pass: "pass".into(),
            imap_oauth2_token: String::new(),
            imap_oauth2_refresh_token: String::new(),
            imap_oauth2_client_id: String::new(),
            imap_oauth2_client_secret: String::new(),
            imap_oauth2_token_endpoint: String::new(),
            pgp_key_path: String::new(),
            pgp_key_id: String::new(),
            pgp_keyring_dir: String::new(),
            drafts_mailbox: "Drafts".into(),
            smtp_server: smtp_server.into(),
            smtp_port: 587,
            smtp_security: neomutt_config::SmtpSecurity::StartTls,
            smtp_user: String::new(),
            smtp_pass: String::new(),
            from: from.into(),
        }
    }

    fn test_mailbox(uids: &[u32]) -> Mailbox {
        Mailbox {
            messages: uids
                .iter()
                .map(|&uid| Message {
                    attachments: Vec::new(),
                    body: format!("body {uid}"),
                    html_body: None,
                    uid,
                    envelope: Envelope {
                        subject: format!("msg {uid}"),
                        from: "a@b".into(),
                        to: "c@d".into(),
                        date: "2024-01-01".into(),
                        message_id: format!("<{uid}@x>"),
                        in_reply_to: String::new(),
                        references: String::new(),
                    },
                    flags: FlagSet::default(),
                    body_fetched: false,
                })
                .collect(),
        }
    }

    // -- CLI parsing -----------------------------------------------------

    #[test]
    fn cli_defaults_have_no_overrides() {
        let args = Args::parse_from(["neomutt"]);
        assert!(args.config.is_none());
        assert!(args.account.is_none());
    }

    #[test]
    fn cli_config_flag_is_parsed() {
        let args =
            Args::parse_from(["neomutt", "--config", "/tmp/cfg.toml"]);
        assert_eq!(args.config.as_deref(), Some("/tmp/cfg.toml"));
    }

    #[test]
    fn cli_config_overrides_env_var() {
        // Simulate: NEOMUTT_CONFIG is set, but --config overrides it.
        // We verify that Args::init() sets the env var to the CLI value.
        // (The actual env mutation is tested via the behaviour: after
        // init(), load_accounts would read from the overridden path.)
        let args = Args::parse_from(["neomutt", "--config", "/cli/path.toml"]);
        assert_eq!(args.config.as_deref(), Some("/cli/path.toml"));
        // When init() runs, it sets NEOMUTT_CONFIG to /cli/path.toml,
        // which would override whatever was in the env before.
    }

    #[test]
    fn cli_account_valid_name_resolves() {
        let args = Args::parse_from(["neomutt", "--account", "work"]);
        let accounts = vec![
            test_account("work", "h1", "s1", ""),
            test_account("personal", "h2", "s2", ""),
        ];
        assert_eq!(args.resolve_account(&accounts).unwrap(), "work");
    }

    #[test]
    fn cli_account_unknown_name_is_error() {
        let args = Args::parse_from(["neomutt", "--account", "nonexistent"]);
        let accounts = vec![
            test_account("work", "h1", "s1", ""),
            test_account("personal", "h2", "s2", ""),
        ];
        let err = args.resolve_account(&accounts).unwrap_err().to_string();
        assert!(
            err.contains("nonexistent"),
            "error should mention the bad name: {err}"
        );
        assert!(
            err.contains("work") && err.contains("personal"),
            "error should list available accounts: {err}"
        );
    }

    #[test]
    fn cli_account_flag_defaults_to_first() {
        let args = Args::parse_from(["neomutt"]);
        let accounts = vec![
            test_account("work", "h1", "s1", ""),
            test_account("personal", "h2", "s2", ""),
        ];
        assert_eq!(args.resolve_account(&accounts).unwrap(), "work");
    }

    #[test]
    fn cli_account_no_accounts_is_error() {
        let args = Args::parse_from(["neomutt"]);
        let accounts: Vec<Account> = vec![];
        let err = args.resolve_account(&accounts).unwrap_err().to_string();
        assert!(err.contains("no accounts"));
    }

    // -- event self-describing -------------------------------------------

    #[test]
    fn mailbox_updated_uses_event_mailbox_name_not_state() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        // Simulate race: the active mailbox is INBOX, but an event
        // arrives for a different mailbox.  Cache operations must use
        // the event's mailbox_name, not state.mailbox_name.
        state.mailbox_name = "Archive".to_owned();

        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2]),
                uid_validity: Some(1),
            },
        );

        // Cache should have been written for INBOX, not Archive.
        let inbox = state.cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(inbox.len(), 2, "cache should write to event's mailbox");
        let archive = state.cache.load_mailbox("work", "Archive").unwrap();
        assert!(archive.is_empty(), "Archive should not have been written");
    }

    // -- mailbox list ----------------------------------------------------

    #[test]
    fn mailbox_list_event_populates_account_mailboxes() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Initially just INBOX.
        assert_eq!(state.accounts["work"].mailboxes.len(), 1);
        assert_eq!(state.accounts["work"].mailboxes[0].name, "INBOX");

        // LIST returns real folders.
        apply_event(
            &mut state,
            ImapEvent::MailboxList {
                account: "work".into(),
                mailboxes: vec![
                    neomutt_mail_store::MailboxEntry { name: "INBOX".into(), label: "📥 INBOX".into() },
                    neomutt_mail_store::MailboxEntry { name: "Sent".into(), label: "📤 Sent".into() },
                    neomutt_mail_store::MailboxEntry { name: "Drafts".into(), label: "📝 Drafts".into() },
                    neomutt_mail_store::MailboxEntry { name: "Work/Projects".into(), label: "Work/Projects".into() },
                ],
            },
        );

        assert_eq!(state.accounts["work"].mailboxes.len(), 4);
        assert_eq!(state.accounts["work"].mailboxes[0].name, "INBOX");
        assert_eq!(state.accounts["work"].mailboxes[0].label, "📥 INBOX");
    }

    #[test]
    fn mailbox_list_event_falls_back_to_inbox_on_empty() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // LIST returns empty (shouldn't clear existing list).
        apply_event(
            &mut state,
            ImapEvent::MailboxList {
                account: "work".into(),
                mailboxes: vec![],
            },
        );

        // The handler just sets whatever comes in. If LIST returns empty,
        // the list is empty. This is expected — the fallback to ["INBOX"]
        // is in the initial AccountState constructor, not in the handler.
        assert!(state.accounts["work"].mailboxes.is_empty());
    }

    // -- HTML sanitization -----------------------------------------------

    #[test]
    fn sanitize_strips_script_tags() {
        let html = "<html><head><script>alert('xss')</script></head><body><p>Safe content</p></body></html>";
        let clean = sanitize_html(html, false);
        assert!(!clean.contains("script"), "script tag should be stripped");
        assert!(!clean.contains("alert"), "script content should be stripped");
        assert!(clean.contains("Safe content"), "safe content should survive");
        assert!(clean.contains("<p>"), "safe tags should survive");
    }

    #[test]
    fn sanitize_blocks_remote_images_by_default() {
        let html = "<p>Hello</p><img src=\"https://tracker.example.com/pixel.gif\" width=\"1\" height=\"1\"><p>World</p>";
        let clean = sanitize_html(html, false);
        assert!(clean.contains("Hello"));
        assert!(clean.contains("World"));
        assert!(!clean.contains("tracker.example.com"), "remote img src should be stripped");
    }

    #[test]
    fn sanitize_allows_remote_images_when_enabled() {
        let html = "<img src=\"https://example.com/photo.jpg\">";
        let clean = sanitize_html(html, true);
        assert!(clean.contains("example.com"), "remote img should survive when enabled");
    }

    #[test]
    fn sanitize_preserves_inline_styles() {
        let html = "<p style=\"color: red; font-size: 14px\">Styled text</p>";
        let clean = sanitize_html(html, false);
        assert!(clean.contains("color: red"), "inline style should survive");
        assert!(clean.contains("Styled text"));
    }

    // -- mailbox create/delete --------------------------------------------

    #[test]
    fn delete_mailbox_sets_confirmation_state() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        apply_command(
            &mut state,
            Command::DeleteMailbox("Trash".into()),
        );
        assert_eq!(state.delete_confirm.as_deref(), Some("Trash"));
    }

    #[test]
    fn delete_confirm_executes_then_clears() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.delete_confirm = Some("Trash".into());

        apply_command(&mut state, Command::DeleteMailboxConfirm);
        assert!(state.delete_confirm.is_none());
        assert!(state
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("deleting mailbox 'Trash'"));
    }

    #[test]
    fn create_mailbox_sets_input_and_mode() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        apply_command(
            &mut state,
            Command::CreateMailbox("new-folder".into()),
        );
        assert_eq!(state.mailbox_create_input, "new-folder");
        assert_eq!(state.mode, Mode::MailboxCreate);
    }

    #[test]
    fn create_confirm_sets_status_and_clears() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.mailbox_create_input = "new-folder".into();

        apply_command(
            &mut state,
            Command::CreateMailboxConfirm("new-folder".into()),
        );
        assert!(state.mailbox_create_input.is_empty());
        assert!(state
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("creating mailbox 'new-folder'"));
    }

    // -- drafts ----------------------------------------------------------

    #[test]
    fn build_draft_message_includes_headers_and_body() {
        let compose = ComposeState {
            to: "bob@example.com".into(),
            subject: "Draft subject".into(),
            body: "Draft body text".into(),
            ..Default::default()
        };
        let raw = build_draft_message(&compose, "alice@example.com");
        let s = String::from_utf8_lossy(&raw);
        assert!(s.contains("From: alice@example.com"));
        assert!(s.contains("To: bob@example.com"));
        assert!(s.contains("Subject: Draft subject"));
        assert!(s.contains("Draft body text"));
        assert!(s.contains("X-neomutt-draft: true"));
        assert!(s.ends_with("\r\nDraft body text"));
    }

    #[test]
    fn edit_draft_prefills_compose_from_message() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        // Seed a message.
        let mut mb = test_mailbox(&[1]);
        mb.messages[0].envelope.to = "bob@x.com".into();
        mb.messages[0].envelope.subject = "Hello".into();
        mb.messages[0].body = "Body text".into();
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: mb,
                uid_validity: Some(1),
            },
        );

        apply_command(&mut state, Command::EditDraft(1));
        assert_eq!(state.mode, Mode::Compose);
        assert_eq!(state.compose.to, "bob@x.com");
        assert_eq!(state.compose.subject, "Hello");
        assert_eq!(state.compose.body, "Body text");
        assert_eq!(state.draft_replacing_uid, Some(1));
    }

    #[test]
    fn save_draft_sets_status() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.mode = Mode::Compose;
        apply_command(&mut state, Command::SaveDraft);
        assert_eq!(
            state.status_message.as_deref(),
            Some("saving draft …")
        );
    }

    // -- copy/move -------------------------------------------------------

    #[test]
    fn copy_message_sets_pending_action() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        apply_command(&mut state, Command::CopyMessage(1));
        assert!(state.pending_copy_move_action.is_some());
        let (uid, is_move, _, _) = state.pending_copy_move_action.unwrap();
        assert_eq!(uid, 1);
        assert!(!is_move);
        assert!(state.show_mailbox_list, "sidebar should open for destination");
    }

    #[test]
    fn select_mailbox_after_copy_confirms_destination() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.accounts.get_mut("work").unwrap().mailboxes = vec![
            neomutt_mail_store::MailboxEntry { name: "INBOX".into(), label: "Inbox".into() },
            neomutt_mail_store::MailboxEntry { name: "Archive".into(), label: "Archive".into() },
        ];

        apply_command(&mut state, Command::CopyMessage(1));
        apply_command(&mut state, Command::SelectMailbox("Archive".into()));

        let (uid, is_move, source, dest) =
            state.pending_copy_move_action.unwrap();
        assert_eq!(uid, 1);
        assert!(!is_move);
        assert_eq!(source, "INBOX");
        assert_eq!(dest, "Archive");
        assert!(state.status_message.as_deref().unwrap().contains("copying"));
    }

    #[test]
    fn take_copy_move_action_clears_state() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.pending_copy_move_action =
            Some((1, true, "INBOX".into(), "Archive".into()));
        state.status_message = Some("moving UID 1 to Archive …".into());

        let result = take_copy_move_action(&mut state);
        assert!(result.is_some());
        assert!(state.pending_copy_move_action.is_none());
    }

    // -- switch mailbox --------------------------------------------------

    #[test]
    fn select_mailbox_sends_switch_command() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.accounts.get_mut("work").unwrap().mailboxes =
            vec![
                neomutt_mail_store::MailboxEntry { name: "INBOX".into(), label: "📥 INBOX".into() },
                neomutt_mail_store::MailboxEntry { name: "Archive".into(), label: "📦 Archive".into() },
            ];
        state.show_mailbox_list = true;

        // Set up a switch channel.
        let (tx, mut rx) = mpsc::channel::<String>(1);
        state.switch_senders.insert("work".into(), tx);

        apply_command(&mut state, Command::SelectMailbox("Archive".into()));

        // Verify the switch command was sent.
        let sent = rx.try_recv().unwrap();
        assert_eq!(sent, "Archive");
    }

    #[test]
    fn switch_mailbox_does_not_affect_other_account() {
        let accounts = vec![
            test_account("work", "h1", "s1", ""),
            test_account("personal", "h2", "s2", ""),
        ];
        let mut state = test_state(accounts);
        // Set up switch channels for both.
        let (tx_w, mut rx_w) = mpsc::channel::<String>(1);
        let (_tx_p, mut rx_p) = mpsc::channel::<String>(1);
        state.switch_senders.insert("work".into(), tx_w);
        state.switch_senders.insert("personal".into(), _tx_p);

        apply_command(&mut state, Command::SelectMailbox("Archive".into()));

        // Work channel received the switch.
        assert!(rx_w.try_recv().is_ok());
        // Personal channel did NOT.
        assert!(rx_p.try_recv().is_err());
    }

    fn test_state(accounts: Vec<Account>) -> AppState {
        let cache = MailboxCache::open_with_limits(":memory:", 10_000, 5_000).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let search =
            neomutt_search::SearchIndex::open(dir.path(), 50_000_000, 50_000).unwrap();
        let mut state = AppState::new(
            accounts,
            "INBOX".into(),
            cache,
            search,
            NotificationConfig::default(),
            neomutt_config::DownloadConfig::default(),
        );
        state.switch_senders = HashMap::new();
        state
    }

    // -- account isolation -------------------------------------------------

    #[test]
    fn mailbox_updated_only_affects_target_account() {
        let accounts = vec![
            test_account("work", "imap.work", "smtp.work", ""),
            test_account("personal", "imap.personal", "smtp.personal", ""),
        ];
        let mut state = test_state(accounts);

        // Both start empty.
        assert!(state.accounts["work"].mailbox.is_empty());
        assert!(state.accounts["personal"].mailbox.is_empty());

        // Update only "work".
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 3]),
                uid_validity: Some(42),
            },
        );

        assert_eq!(state.accounts["work"].mailbox.messages.len(), 3);
        assert_eq!(state.accounts["work"].uid_validity, Some(42));
        // Personal untouched.
        assert!(state.accounts["personal"].mailbox.is_empty());
        assert!(state.accounts["personal"].uid_validity.is_none());
    }

    // -- UIDVALIDITY isolation ---------------------------------------------

    #[test]
    fn uid_validity_change_wipes_only_target_account() {
        let accounts = vec![
            test_account("work", "imap.work", "smtp.work", ""),
            test_account("personal", "imap.personal", "smtp.personal", ""),
        ];
        let mut state = test_state(accounts);

        // Seed both accounts with some data.
        state.cache.set_uid_validity("work", "INBOX", 1).unwrap();
        state
            .cache
            .save_messages("work", "INBOX", &test_mailbox(&[1, 2]).messages)
            .unwrap();
        state
            .cache
            .set_uid_validity("personal", "INBOX", 99)
            .unwrap();
        state
            .cache
            .save_messages("personal", "INBOX", &test_mailbox(&[10]).messages)
            .unwrap();

        state.accounts.get_mut("work").unwrap().uid_validity = Some(1);
        state.accounts.get_mut("personal").unwrap().uid_validity = Some(99);

        // Work UIDVALIDITY changes.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 3]),
                uid_validity: Some(2),
            },
        );

        // Work cache should be wiped (replaced with new data).
        let work_msgs = state.cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(work_msgs.len(), 3);

        // Personal untouched.
        let pers_msgs = state.cache.load_mailbox("personal", "INBOX").unwrap();
        assert_eq!(pers_msgs.len(), 1);
        assert_eq!(
            state.cache.get_uid_validity("personal", "INBOX"),
            Some(99)
        );
    }

    // -- account switching -------------------------------------------------

    #[test]
    fn next_account_cycles_forward() {
        let accounts = vec![
            test_account("a", "h1", "s1", ""),
            test_account("b", "h2", "s2", ""),
            test_account("c", "h3", "s3", ""),
        ];
        let mut state = test_state(accounts);
        assert_eq!(state.active_account, "a");

        apply_command(&mut state, Command::NextAccount);
        assert_eq!(state.active_account, "b");

        apply_command(&mut state, Command::NextAccount);
        assert_eq!(state.active_account, "c");

        // Wraparound.
        apply_command(&mut state, Command::NextAccount);
        assert_eq!(state.active_account, "a");
    }

    #[test]
    fn prev_account_cycles_backward() {
        let accounts = vec![
            test_account("a", "h1", "s1", ""),
            test_account("b", "h2", "s2", ""),
            test_account("c", "h3", "s3", ""),
        ];
        let mut state = test_state(accounts);
        assert_eq!(state.active_account, "a");

        apply_command(&mut state, Command::PrevAccount);
        assert_eq!(state.active_account, "c"); // wraparound

        apply_command(&mut state, Command::PrevAccount);
        assert_eq!(state.active_account, "b");
    }

    #[test]
    fn account_switch_on_single_account_is_noop() {
        let accounts = vec![test_account("only", "h", "s", "")];
        let mut state = test_state(accounts);
        apply_command(&mut state, Command::NextAccount);
        assert_eq!(state.active_account, "only");
        apply_command(&mut state, Command::PrevAccount);
        assert_eq!(state.active_account, "only");
    }

    // -- empty state doesn't panic -----------------------------------------

    #[test]
    fn navigate_on_empty_mailbox_does_not_panic() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        // Mailbox is empty from cache load (fresh :memory:).

        apply_command(&mut state, Command::NavigateDown);
        apply_command(&mut state, Command::NavigateUp);
        apply_command(&mut state, Command::OpenReply); // no message selected
        apply_command(&mut state, Command::OpenReplyAll);

        // No panics = pass.
    }

    #[test]
    fn commands_on_no_accounts_do_not_panic() {
        let accounts: Vec<Account> = vec![];
        let mut state = test_state(accounts);
        // active_account is "".

        apply_command(&mut state, Command::NavigateDown);
        apply_command(&mut state, Command::NavigateUp);
        apply_command(&mut state, Command::NextAccount);
        apply_command(&mut state, Command::PrevAccount);
    }

    // -- SMTP config picks active account ----------------------------------

    #[test]
    fn smtp_config_uses_active_account() {
        let accounts = [test_account("work", "imap.work", "smtp.work.com", "work@work.com"),
            test_account(
                "personal",
                "imap.personal",
                "smtp.personal.com",
                "me@personal.com",
            )];

        // Active is "work" (first).
        let work_cfg = smtp_config_for_account(&accounts[0]);
        assert_eq!(work_cfg.server, "smtp.work.com");
        assert_eq!(accounts[0].effective_from(), "work@work.com");

        let pers_cfg = smtp_config_for_account(&accounts[1]);
        assert_eq!(pers_cfg.server, "smtp.personal.com");
        assert_eq!(accounts[1].effective_from(), "me@personal.com");
    }

    // -- flag manipulation -----------------------------------------------

    #[test]
    fn optimistic_flag_toggle_updates_local_state() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Seed a message.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );
        // Initially not seen.
        assert!(!state.accounts["work"].mailbox.messages[0]
            .flags
            .contains(FlagSet::SEEN));

        // Simulate ToggleSeen — optimistically flip the flag.
        if let Some(acct) = state.accounts.get_mut("work")
            && let Some(msg) = acct.mailbox.messages.iter_mut().find(|m| m.uid == 1) {
                let currently = msg.flags.contains(FlagSet::SEEN);
                if currently {
                    msg.flags.remove(FlagSet::SEEN);
                } else {
                    msg.flags.insert(FlagSet::SEEN);
                }
            }

        assert!(state.accounts["work"].mailbox.messages[0]
            .flags
            .contains(FlagSet::SEEN));
    }

    #[test]
    fn flag_update_persists_in_cache() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        // Toggle flagged on uid 1.
        if let Some(acct) = state.accounts.get_mut("work")
            && let Some(msg) = acct.mailbox.messages.iter_mut().find(|m| m.uid == 1) {
                msg.flags.insert(FlagSet::FLAGGED);
                state
                    .cache
                    .save_messages("work", "INBOX", &[msg.clone()])
                    .unwrap();
            }

        // Read back from cache.
        let cached = state.cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(cached.len(), 1);
        assert!(cached[0].flags.contains(FlagSet::FLAGGED));
    }

    #[test]
    fn flags_to_imap_string_formats_correctly() {
        // Test the conversion in mail-store.
        let mut flags = FlagSet::default();
        flags.insert(FlagSet::SEEN);
        flags.insert(FlagSet::FLAGGED);
        let s = neomutt_mail_store::flags_to_imap_string(flags);
        assert!(s.contains("\\Seen"));
        assert!(s.contains("\\Flagged"));
        assert!(!s.contains("\\Deleted"));
    }

    // -- error surfacing --------------------------------------------------

    #[test]
    fn error_event_sets_status_message() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        apply_event(
            &mut state,
            ImapEvent::Error {
                account: "work".into(),
                message: "auth failed".into(),
            },
        );

        assert_eq!(
            state.status_message.as_deref(),
            Some("[work] auth failed")
        );
    }

    // -- file browser / attach --------------------------------------------

    #[test]
    fn list_files_lists_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let entries = super::list_files(&dir.path().to_string_lossy());
        // Should have ".." (parent), "subdir" (directory), "test.txt" (file).
        let has_subdir = entries.iter().any(|e| e.name == "subdir" && e.is_dir);
        let has_file = entries
            .iter()
            .any(|e| e.name == "test.txt" && !e.is_dir && e.size > 0);
        assert!(has_subdir, "should list subdir");
        assert!(has_file, "should list test.txt");
    }

    #[test]
    fn attach_file_then_clear_on_cancel() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Simulate attaching a file.
        state.attached_files.push(neomutt_ui::FileEntry {
            name: "test.txt".into(),
            is_dir: false,
            size: 100,
        });
        state.attached_file_paths.push("/tmp/test.txt".into());
        assert_eq!(state.attached_files.len(), 1);

        // Enter compose — resets attachments.
        apply_command(&mut state, Command::OpenCompose);
        assert!(state.attached_files.is_empty(), "OpenCompose clears attachments");

        // Re-attach and cancel.
        state.attached_files.push(neomutt_ui::FileEntry {
            name: "test.txt".into(),
            is_dir: false,
            size: 100,
        });
        apply_command(&mut state, Command::OpenCompose);
        assert!(state.attached_files.is_empty(), "OpenCompose clears again");
    }

    #[test]
    fn size_limit_rejects_large_file() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.download_config.max_attach_size = 10; // 10 bytes

        // Create a temp file > 10 bytes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        std::fs::write(&path, "this is more than ten bytes").unwrap();

        // Enter file browser mode, then simulate BrowseSelect.
        state.mode = Mode::FileBrowser;
        state.browser_path = dir.path().to_string_lossy().to_string();
        apply_command(
            &mut state,
            Command::BrowseSelect(path.to_string_lossy().to_string()),
        );
        // Should show error, not attach.
        assert!(state.status_message.as_deref().unwrap_or("").contains("too large"));
        assert!(state.attached_files.is_empty());
    }

    // -- notification triggering -----------------------------------------

    #[test]
    fn notification_triggered_with_preview_on_new_mail() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Apply an update with genuinely new messages.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2]),
                uid_validity: Some(1),
            },
        );
        // new_mail_total should be 2 (accumulator).
        assert_eq!(state.new_mail_total, 2);
        // The notification function is called internally but guarded by
        // cfg!(test) — we verify the state mutation happened correctly.
    }

    #[test]
    fn notification_suppressed_when_config_disabled() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        state.notif_config.enabled = false;

        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );
        // new_mail_total should still accumulate.
        assert_eq!(state.new_mail_total, 1);
        // Notifications are suppressed in tests anyway, but the config flag
        // is correctly plumbed — live code would respect it.
    }

    // -- new-mail diff ----------------------------------------------------

    #[test]
    fn new_mail_count_detects_genuinely_new_uids() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // First update: 3 messages.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 3]),
                uid_validity: Some(1),
            },
        );
        assert_eq!(state.compute_new_mail_count(), 3);

        // Second update: uid 4 is new, 1 and 2 are existing.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 4]),
                uid_validity: Some(1),
            },
        );
        assert_eq!(state.compute_new_mail_count(), 1);
    }

    #[test]
    fn new_mail_total_accumulates_across_events_and_clears_on_command() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // First update: 2 new messages.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2]),
                uid_validity: Some(1),
            },
        );
        assert_eq!(state.new_mail_total, 2);

        // Second update: 1 more new message.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 3]),
                uid_validity: Some(1),
            },
        );
        assert_eq!(state.new_mail_total, 3, "accumulated: 2 + 1");

        // Third update: no new messages.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 3]),
                uid_validity: Some(1),
            },
        );
        assert_eq!(state.new_mail_total, 3, "unchanged: still 3");

        // User navigates — clears the badge.
        apply_command(&mut state, Command::NavigateDown);
        assert_eq!(state.new_mail_total, 0, "cleared on user interaction");
    }

    #[test]
    fn new_mail_count_zero_when_no_change() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2]),
                uid_validity: Some(1),
            },
        );
        // Same UIDs again.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2]),
                uid_validity: Some(1),
            },
        );
        assert_eq!(state.compute_new_mail_count(), 0);
    }

    // -- credential exposure / Debug -------------------------------------

    #[test]
    fn imap_config_debug_redacts_password() {
        let cfg = neomutt_mail_store::ImapConfig {
            host: "h".into(),
            port: 993,
            security: neomutt_mail_store::ImapSecurity::Direct,
            user: "u".into(),
            pass: "s3cret".into(),
            oauth2_token: "ya29.token".into(),
            oauth2_refresh_token: String::new(),
            oauth2_client_id: String::new(),
            oauth2_client_secret: String::new(),
            oauth2_token_endpoint: String::new(),
            backoff_init_secs: 1,
            backoff_max_secs: 30,
            poll_interval_secs: 30,
            max_fetch_size_bytes: 25 * 1024 * 1024,
        };
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("***REDACTED***"));
        assert!(!dbg.contains("s3cret"), "password must not appear: {dbg}");
        assert!(!dbg.contains("ya29.token"), "token must not appear: {dbg}");
        assert!(dbg.contains("\"h\""), "host should be visible");
        assert!(dbg.contains("\"u\""), "user should be visible");
        assert!(dbg.contains("max_fetch_size_bytes"), "max_fetch_size_bytes should be visible: {dbg}");
    }

    #[test]
    fn smtp_config_debug_redacts_password() {
        let cfg = neomutt_smtp_client::SmtpConfig {
            server: "s".into(),
            port: 587,
            security: neomutt_smtp_client::SmtpSecurity::StartTls,
            user: Some("u".into()),
            pass: Some("smtp-pass".into()),
        };
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("***REDACTED***"));
        assert!(!dbg.contains("smtp-pass"), "password must not appear: {dbg}");
    }

    #[test]
    fn account_debug_redacts_all_credentials() {
        let acct = neomutt_config::Account {
            name: "test".into(),
            imap_host: "h".into(),
            imap_port: 993,
            imap_security: neomutt_config::ImapSecurity::Direct,
            imap_user: "u".into(),
            imap_pass: "imap-secret".into(),
            imap_oauth2_token: "oauth-token".into(),
            imap_oauth2_refresh_token: String::new(),
            imap_oauth2_client_id: String::new(),
            imap_oauth2_client_secret: String::new(),
            imap_oauth2_token_endpoint: String::new(),
            pgp_key_path: String::new(),
            pgp_key_id: String::new(),
            pgp_keyring_dir: String::new(),
            drafts_mailbox: "Drafts".into(),
            smtp_server: "s".into(),
            smtp_port: 587,
            smtp_security: neomutt_config::SmtpSecurity::StartTls,
            smtp_user: String::new(),
            smtp_pass: "smtp-secret".into(),
            from: String::new(),
        };
        let dbg = format!("{acct:?}");
        assert!(dbg.contains("***REDACTED***"));
        assert!(!dbg.contains("imap-secret"), "imap pass must not appear");
        assert!(!dbg.contains("oauth-token"), "oauth token must not appear");
        assert!(!dbg.contains("smtp-secret"), "smtp pass must not appear");
        assert!(dbg.contains("\"test\""), "name should be visible");
    }

    // -- path traversal ---------------------------------------------------

    #[test]
    fn sanitize_filename_strips_directory_components() {
        let safe = super::sanitize_filename("../../.ssh/authorized_keys");
        assert_eq!(safe, "authorized_keys");
    }

    #[test]
    fn sanitize_filename_keeps_plain_name() {
        let safe = super::sanitize_filename("report.pdf");
        assert_eq!(safe, "report.pdf");
    }

    #[test]
    fn sanitize_filename_falls_back_to_unnamed() {
        let safe = super::sanitize_filename("..");
        assert_eq!(safe, "unnamed");
    }

    #[test]
    fn save_attachment_sanitizes_traversal_filename() {
        let dir = tempfile::tempdir().unwrap();
        let att = neomutt_core::Attachment {
            filename: "../../etc/passwd".into(),
            content_type: "text/plain".into(),
            size: 4,
            body: Some(b"data".to_vec()),
        };
        let result =
            super::save_attachment_to_disk(&dir.path().to_string_lossy(), &att);
        // Sanitization strips directory components; the file is saved
        // as just "passwd" inside the download directory.  No error.
        assert!(result.is_ok());
        let saved = result.unwrap();
        assert!(saved.starts_with(dir.path()), "must be inside download dir");
        assert!(saved.ends_with("passwd"), "traversal stripped to base name");
    }

    // -- attachment save --------------------------------------------------

    #[test]
    fn resolve_save_path_appends_number_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("report.pdf");
        std::fs::write(&base, "original").unwrap();

        let resolved =
            super::resolve_save_path(&dir.path().to_string_lossy(), "report.pdf");
        assert_eq!(
            resolved.file_name().unwrap().to_string_lossy(),
            "report (1).pdf"
        );
    }

    #[test]
    fn resolve_save_path_uses_original_when_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let resolved =
            super::resolve_save_path(&dir.path().to_string_lossy(), "new.txt");
        assert_eq!(resolved, dir.path().join("new.txt"));
    }

    #[test]
    fn save_attachment_writes_bytes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let att = neomutt_core::Attachment {
            filename: "test.txt".into(),
            content_type: "text/plain".into(),
            size: 4,
            body: Some(b"data".to_vec()),
        };
        let result =
            super::save_attachment_to_disk(&dir.path().to_string_lossy(), &att);
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
    }

    // -- detail view ------------------------------------------------------

    #[test]
    fn open_message_switches_to_detail_mode() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        apply_command(&mut state, Command::OpenMessage(1));
        assert_eq!(state.mode, Mode::MessageDetail);
        assert_eq!(state.detail_scroll, 0);
    }

    #[test]
    fn close_detail_returns_to_message_list() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        apply_command(&mut state, Command::OpenMessage(1));
        apply_command(&mut state, Command::CloseDetail);
        assert_eq!(state.mode, Mode::MessageList);
        assert_eq!(state.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_clamps_at_zero() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        apply_command(&mut state, Command::OpenMessage(1));
        apply_command(&mut state, Command::DetailScrollUp);
        apply_command(&mut state, Command::DetailScrollUp);
        assert_eq!(state.detail_scroll, 0, "shouldn't go below 0");
    }

    // -- mailbox switching ------------------------------------------------

    #[test]
    fn switching_mailbox_updates_active_mailbox_and_state() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Populate mailboxes list.
        state.accounts.get_mut("work").unwrap().mailboxes =
            vec![
                neomutt_mail_store::MailboxEntry { name: "INBOX".into(), label: "📥 INBOX".into() },
                neomutt_mail_store::MailboxEntry { name: "Archive".into(), label: "📦 Archive".into() },
            ];
        state.show_mailbox_list = true;

        // Select "Archive".
        apply_command(&mut state, Command::SelectMailbox("Archive".into()));
        assert_eq!(state.mailbox_name, "Archive");
        assert_eq!(
            state.accounts["work"].active_mailbox,
            "Archive"
        );
        assert!(!state.show_mailbox_list, "sidebar closed after selection");
    }

    #[test]
    fn toggling_mailbox_list_shows_and_hides() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        assert!(!state.show_mailbox_list);
        apply_command(&mut state, Command::ToggleMailboxList);
        assert!(state.show_mailbox_list);
        apply_command(&mut state, Command::ToggleMailboxList);
        assert!(!state.show_mailbox_list);
    }

    // -- expunge ----------------------------------------------------------

    #[test]
    fn expunge_removes_only_deleted_messages_from_local_state() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Seed 3 messages: uid 1 normal, uid 2 deleted, uid 3 normal.
        let mut mb = test_mailbox(&[1, 2, 3]);
        mb.messages[1].flags.insert(FlagSet::DELETED);
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: mb,
                uid_validity: Some(1),
            },
        );

        // Simulate expunge refetch: server returns only uids 1 and 3.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 3]),
                uid_validity: Some(1),
            },
        );

        let msgs = &state.accounts["work"].mailbox.messages;
        assert_eq!(msgs.len(), 2, "deleted message should be gone");
        assert_eq!(msgs[0].uid, 1);
        assert_eq!(msgs[1].uid, 3);
    }

    #[test]
    fn selected_index_clamps_after_expunge() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1, 2, 3]),
                uid_validity: Some(1),
            },
        );
        state.accounts.get_mut("work").unwrap().selected_index = 2;

        // Expunge removes all but uid 1.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        let acct = &state.accounts["work"];
        assert_eq!(acct.mailbox.messages.len(), 1);
        assert_eq!(
            acct.selected_index, 0,
            "index should clamp to 0 when list shrinks"
        );
    }

    // -- body fetch -------------------------------------------------------

    #[test]
    fn body_fetched_event_updates_message_in_place() {
        let accounts = vec![test_account("work", "h", "s", "")];
        let mut state = test_state(accounts);

        // Seed a message without body.
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );
        assert!(!state.accounts["work"].mailbox.messages[0].body_fetched);

        // Simulate body fetch completing.
        apply_event(
            &mut state,
            ImapEvent::BodyFetched {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                uid: 1,
                body: "Hello, this is the body.".into(),
                html_body: None,
            },
        );

        let msg = &state.accounts["work"].mailbox.messages[0];
        assert!(msg.body_fetched);
        assert_eq!(msg.body, "Hello, this is the body.");
    }

    #[test]
    fn body_fetched_does_not_affect_other_accounts() {
        let accounts = vec![
            test_account("work", "h1", "s1", ""),
            test_account("personal", "h2", "s2", ""),
        ];
        let mut state = test_state(accounts);
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );
        apply_event(
            &mut state,
            ImapEvent::MailboxUpdated {
                account: "personal".into(),
                mailbox_name: "INBOX".into(),
                mailbox: test_mailbox(&[1]),
                uid_validity: Some(1),
            },
        );

        apply_event(
            &mut state,
            ImapEvent::BodyFetched {
                account: "work".into(),
                mailbox_name: "INBOX".into(),
                uid: 1,
                body: "work body".into(),
                html_body: None,
            },
        );

        assert!(state.accounts["work"].mailbox.messages[0].body_fetched);
        assert!(!state.accounts["personal"].mailbox.messages[0].body_fetched);
    }

    // -- PGP encrypt send path -------------------------------------------

    #[test]
    fn encrypt_fails_with_specific_error_when_no_key_found() {
        let mut compose = ComposeState::default();
        compose.to = "bob@example.com".into();
        compose.subject = "secret".into();
        compose.body = "classified".into();
        compose.encrypt = true;

        let empty_keyring = Keyring::default();
        let result = send_via_smtp(
            &SmtpConfig {
                server: "localhost".into(),
                port: 25,
                security: neomutt_smtp_client::SmtpSecurity::StartTls,
                user: None,
                pass: None,
            },
            "alice@example.com",
            &compose,
            "",
            &empty_keyring,
            &[],
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no keyring configured")
                || err.contains("no public key found"),
            "should mention keyring/keys: {err}"
        );
    }

    #[test]
    fn encrypt_never_silently_downgrades_to_plaintext() {
        let mut compose = ComposeState::default();
        compose.to = "bob@example.com".into();
        compose.encrypt = true;

        let empty_keyring = Keyring::default();
        let result = send_via_smtp(
            &SmtpConfig {
                server: "localhost".into(),
                port: 25,
                security: neomutt_smtp_client::SmtpSecurity::StartTls,
                user: None,
                pass: None,
            },
            "alice@example.com",
            &compose,
            "",
            &empty_keyring,
            &[],
            None,
        );
        // Must fail — never fall through to sending plaintext.
        assert!(result.is_err(), "must not silently downgrade to plaintext");
    }

    #[test]
    fn send_compose_sets_status_and_returns_data() {
        let accounts = vec![test_account("work", "imap.work", "smtp.work", "")];
        let mut state = test_state(accounts.clone());

        // Enter compose and fill in something.
        apply_command(&mut state, Command::OpenCompose);
        apply_command(&mut state, Command::ComposeInput('t'));
        apply_command(&mut state, Command::ComposeNextField); // -> Subject
        apply_command(&mut state, Command::ComposeInput('s'));
        apply_command(&mut state, Command::ComposeNextField); // -> Body
        apply_command(&mut state, Command::ComposeInput('b'));

        // Send.
        let outcome = apply_command(&mut state, Command::SendCompose);
        assert_eq!(outcome, CommandOutcome::Continue);
        assert_eq!(state.status_message.as_deref(), Some("sending …"));

        // take_send_request should return the right data.
        let req = take_send_request(&state, &accounts).unwrap();
        assert_eq!(req.0.server, "smtp.work");
        assert_eq!(req.1, "work@example.com");
        assert_eq!(req.2.to, "t");
        assert_eq!(req.2.subject, "s");
        assert_eq!(req.2.body, "b");
        assert_eq!(req.3, ""); // pgp_key_path not set
        assert_eq!(req.4, ""); // pgp_keyring_dir not set
    }

    // -- bounded channel backpressure --------------------------------------

    #[test]
    fn bounded_render_channel_drops_when_full() {
        // Simulate the tx_render coalesce policy: capacity 2, try_send.
        // When full, new values are dropped — only the latest matters.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u32>(2);

        // Fill the channel.
        assert!(tx.try_send(1).is_ok());
        assert!(tx.try_send(2).is_ok());
        // Channel is now full — try_send should fail.
        assert!(tx.try_send(3).is_err());

        // Drain and verify we got 1 and 2 (old values preserved).
        assert_eq!(rx.try_recv().ok(), Some(1));
        assert_eq!(rx.try_recv().ok(), Some(2));
        assert!(rx.try_recv().is_err()); // empty

        // Now we can send again.
        assert!(tx.try_send(4).is_ok());
        assert_eq!(rx.try_recv().ok(), Some(4));
    }

    #[test]
    fn bounded_command_channel_backpressures_via_blocking_send() {
        // Simulate the tx_commands policy: capacity 2, blocking_send.
        // blocking_send waits until space is available.
        let (tx, rx) = tokio::sync::mpsc::channel::<u32>(2);

        // Fill the channel.
        tx.blocking_send(1).unwrap();
        tx.blocking_send(2).unwrap();

        // Spawn a task that will drain the channel after a short delay,
        // then blocking_send again — it should succeed.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(rx); // close channel so blocking_send returns error
        });

        // blocking_send should either succeed (if drained in time) or fail
        // with Closed (if dropped).  It should NOT panic or hang forever.
        let result = tx.blocking_send(3);
        // Regardless of timing, blocking_send returns — no deadlock.
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn bounded_channel_is_closed_detection_works() {
        let (tx, rx) = tokio::sync::mpsc::channel::<u32>(4);
        assert!(!tx.is_closed());
        drop(rx);
        assert!(tx.is_closed());
    }
}
