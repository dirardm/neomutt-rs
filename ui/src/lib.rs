//! ratatui + crossterm terminal UI for neomutt-rs.
//!
//! The UI is a pure function of [`RenderState`] — it doesn't own or mutate
//! any mailbox data.  Keypresses produce [`Command`] values that the App
//! State task consumes.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc;

use neomutt_core::thread::ThreadNode;
use neomutt_core::{FlagSet, Mailbox, Message};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which view the UI is currently showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    MessageList,
    Search,
    Compose,
    MessageDetail,
    FileBrowser,
    MailboxCreate,
    PassphrasePrompt,
}

/// A single entry in the file browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Which field in the compose form has focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposeField {
    #[default]
    To,
    Subject,
    Body,
}

impl ComposeField {
    pub fn next(self) -> Self {
        match self {
            ComposeField::To => ComposeField::Subject,
            ComposeField::Subject => ComposeField::Body,
            ComposeField::Body => ComposeField::To,
        }
    }
}

/// In-progress compose state owned by App State.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposeState {
    pub to: String,
    pub subject: String,
    pub body: String,
    pub active_field: ComposeField,
    /// RFC 5322 In-Reply-To (Message-ID of the message being replied to).
    pub in_reply_to: Option<String>,
    /// RFC 5322 References chain.
    pub references: Option<String>,
    /// Whether to PGP-sign the message before sending.
    pub sign: bool,
    /// Whether to PGP-encrypt the message before sending.
    pub encrypt: bool,
}

/// User actions the UI produces — consumed by the App State task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    // -- navigation in message list ------------------------------------------
    NavigateUp,
    NavigateDown,

    // -- compose mode entry --------------------------------------------------
    OpenCompose,
    OpenReply,
    OpenReplyAll,

    // -- search --------------------------------------------------------------
    OpenSearch,
    SearchInput(char),
    SearchBackspace,
    RunSearch,
    CancelSearch,

    // -- message body fetch --------------------------------------------------
    OpenMessage(u32),

    // -- detail view ---------------------------------------------------------
    CloseDetail,
    DetailScrollUp,
    DetailScrollDown,
    DetailAttachNext,
    SaveAttachment,
    OpenInBrowser,

    // -- flag manipulation ---------------------------------------------------
    ToggleSeen(u32),
    ToggleFlagged(u32),
    Delete(u32),
    Expunge,

    // -- compose editing -----------------------------------------------------
    CancelCompose,
    SendCompose,
    SaveDraft,
    EditDraft(u32),
    ComposeInput(char),
    ComposeBackspace,
    ComposeNewline,
    ComposeNextField,
    ToggleSign,
    ToggleEncrypt,

    // -- file browser --------------------------------------------------------
    OpenFileBrowser,
    BrowseCancel,
    BrowseDir(String),
    BrowseSelect(String),
    BrowseUp,
    BrowseDown,

    // -- view toggles --------------------------------------------------------
    ToggleThreaded,

    // -- account switching ---------------------------------------------------
    NextAccount,
    PrevAccount,

    // -- mailbox list ---------------------------------------------------------
    ToggleMailboxList,
    SelectMailbox(String),
    CreateMailbox(String),
    CreateMailboxConfirm(String),
    DeleteMailbox(String),
    DeleteMailboxConfirm,

    // -- copy/move ------------------------------------------------------------
    CopyMessage(u32),
    MoveMessage(u32),

    // -- passphrase prompt ---------------------------------------------------
    /// Submit typed passphrase (actual text, never logged).
    PassphraseSubmit(String),
    /// Cancel passphrase entry.
    PassphraseCancel,

    // -- global --------------------------------------------------------------
    Quit,
}

/// A render-safe passphrase request — the prompt text is shown, but the
/// actual passphrase goes through a oneshot channel, never via RenderState.
pub struct PassphraseRequest {
    /// Display text (e.g. "Enter passphrase for PGP signing key")
    pub prompt: String,
    /// Number of characters typed so far (for masked display).
    pub masked_len: usize,
    /// The submit channel — caller awaits this.
    #[allow(clippy::type_complexity)]
    pub submit: Option<tokio::sync::oneshot::Sender<String>>,
}

/// A single row in a threaded message list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadEntry {
    pub uid: u32,
    /// Indentation level (0 = root).
    pub depth: usize,
    /// Whether this message or any of its descendants is unread.
    pub has_unseen: bool,
}

/// Snapshot of state the UI needs in order to draw one frame.
#[derive(Clone, Debug)]
pub struct RenderState {
    pub mode: Mode,
    pub mailbox_name: String,
    pub mailbox: Mailbox,
    pub selected_index: usize,
    pub compose: ComposeState,
    /// Transient status line (e.g. "sending …", "sent ✓", "failed ✗").
    pub status_message: Option<String>,
    /// Whether the user has toggled threaded view.
    pub threaded: bool,
    /// Pre-computed flat thread entries (filled when `threaded` is true).
    pub thread_entries: Vec<ThreadEntry>,
    /// Current search query (shown in search bar when mode == Search).
    pub search_query: String,
    /// UIDs matching the current search, in relevance order.
    pub search_uids: Vec<u32>,
    /// Autocomplete results for the compose To field.
    pub contacts: Vec<neomutt_cache::Contact>,
    /// Count of genuinely new messages since last update (for badge).
    pub new_mail_count: usize,
    /// Scroll position in the detail view body pane.
    pub detail_scroll: usize,
    /// Currently selected attachment index in the detail view.
    pub detail_attach_index: usize,
    /// File browser: current directory path.
    pub browser_path: String,
    /// File browser: entries in the current directory.
    pub browser_files: Vec<FileEntry>,
    /// File browser: selected entry index.
    pub browser_index: usize,
    /// Compose: files attached so far.
    pub attached_files: Vec<FileEntry>,
    /// Input for creating a new mailbox.
    pub mailbox_create_input: String,
    /// Active passphrase prompt (if mode == PassphrasePrompt).
    pub passphrase_prompt: Option<String>,
    /// Number of masked characters typed.
    pub passphrase_masked_len: usize,

    /// Delete confirmation pending: name of mailbox to delete.
    pub delete_confirm: Option<String>,
    /// Whether to show the mailbox list sidebar.
    pub show_mailbox_list: bool,
    /// Available mailbox names for the active account.
    pub mailboxes: Vec<neomutt_mail_store::MailboxEntry>,
    /// Selected index in the mailbox list.
    pub mailbox_list_index: usize,
    /// Custom keybindings from config: key-combo → command name.
    pub keybindings: HashMap<String, String>,
    /// Message list column widths.
    pub column_widths: (usize, usize, usize), // (subject, from, date)
}

// ---------------------------------------------------------------------------
// Reply helpers
// ---------------------------------------------------------------------------

/// Create a pre-filled [`ComposeState`] for replying to `original`.
///
/// * `reply_all` — when `true`, the `To` field includes all original
///   recipients in addition to the sender.
pub fn reply_compose_state(
    original: &neomutt_core::Message,
    reply_all: bool,
) -> ComposeState {
    let to = if reply_all {
        // Best-effort reply-all: sender + original To list.
        let mut recipients = original.envelope.from.clone();
        if !original.envelope.to.is_empty() {
            recipients.push_str(", ");
            recipients.push_str(&original.envelope.to);
        }
        recipients
    } else {
        original.envelope.from.clone()
    };

    let subject = reply_subject(&original.envelope.subject);

    let body = format!(
        "\n\nOn {}, {} wrote:\n",
        original.envelope.date, original.envelope.from
    );

    ComposeState {
        to,
        subject,
        body,
        active_field: ComposeField::Body, // cursor starts in body for replies
        in_reply_to: Some(original.envelope.message_id.clone()),
        references: Some(original.envelope.message_id.clone()),
        sign: false,
        encrypt: false,
    }
}

/// Build a "Re: " subject, avoiding double-prefixing.
fn reply_subject(original: &str) -> String {
    if original.to_lowercase().starts_with("re:") {
        original.to_owned()
    } else {
        format!("Re: {}", original)
    }
}

// ---------------------------------------------------------------------------
// Thread flattening
// ---------------------------------------------------------------------------

/// Flatten a [`ThreadNode`] tree into a display-order list of
/// [`ThreadEntry`] values suitable for rendering in the message list.
pub fn flatten_thread(roots: &[ThreadNode], messages: &[Message]) -> Vec<ThreadEntry> {
    // Build a lookup from UID → message for fast SEEN checks.
    let msg_map: std::collections::HashMap<u32, &Message> =
        messages.iter().map(|m| (m.uid, m)).collect();

    let mut entries = Vec::new();
    for root in roots {
        flatten_node(root, 0, &msg_map, &mut entries);
    }
    entries
}

fn flatten_node(
    node: &ThreadNode,
    depth: usize,
    msg_map: &std::collections::HashMap<u32, &Message>,
    out: &mut Vec<ThreadEntry>,
) {
    if let Some(uid) = node.uid {
        let has_unseen = !msg_map
            .get(&uid)
            .map(|m| m.flags.contains(FlagSet::SEEN))
            .unwrap_or(true);
        out.push(ThreadEntry {
            uid,
            depth,
            has_unseen,
        });
    }
    for child in &node.children {
        flatten_node(child, depth + 1, msg_map, out);
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the full TUI into `frame`.
pub fn render(frame: &mut Frame, state: &RenderState) {
    match state.mode {
        Mode::FileBrowser => {
            let area = frame.area();
            let items: Vec<ListItem<'_>> = state
                .browser_files
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let icon = if e.is_dir { "📁" } else { "📄" };
                    let prefix = if i == state.browser_index { "▶ " } else { "  " };
                    Line::from(format!(
                        "{prefix}{icon} {}  ({} bytes)",
                        e.name, e.size
                    ))
                    .into()
                })
                .collect();
            let list = List::new(items)
                .block(
                    Block::default()
                        .title(format!(" Attach file — {}", state.browser_path))
                        .borders(Borders::ALL),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD),
                );
            frame.render_stateful_widget(list, area, &mut file_browser_state(state.browser_index));
        }
        Mode::MessageDetail => {
            let [body_area, status_area] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)])
                    .areas(frame.area());
            render_detail_view(frame, body_area, state);
            render_detail_help(frame, status_area);
        }
        Mode::MessageList | Mode::Search | Mode::MailboxCreate => {
            let areas = if state.show_mailbox_list {
                let [sidebar, main] =
                    Layout::horizontal([Constraint::Length(20), Constraint::Min(0)])
                        .areas(frame.area());
                render_mailbox_sidebar(frame, sidebar, state);
                main
            } else {
                frame.area()
            };
            let [list_area, search_area, status_area] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1), Constraint::Length(1)])
                    .areas(areas);
            render_message_list(frame, list_area, state);
            if state.mode == Mode::Search {
                render_search_bar(frame, search_area, state);
            }
            render_status_bar(frame, status_area, state);
        }
        Mode::Compose => {
            let [body_area, status_area] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)])
                    .areas(frame.area());
            render_compose(frame, body_area, state);
            render_compose_help(frame, status_area, state);
        }
        Mode::PassphrasePrompt => {
            let area = frame.area();
            let popup = ratatui::layout::Rect {
                x: area.width / 4,
                y: area.height / 3,
                width: area.width / 2,
                height: 5,
            };
            let block = ratatui::widgets::Block::default()
                .title(state.passphrase_prompt.as_deref().unwrap_or("Passphrase"))
                .borders(ratatui::widgets::Borders::ALL)
                .style(ratatui::style::Style::default().bg(ratatui::style::Color::DarkGray));
            let masked = "*".repeat(state.passphrase_masked_len);
            let input_line = ratatui::text::Line::from(vec![
                ratatui::text::Span::raw("  "),
                ratatui::text::Span::styled(
                    format!("{}▎", masked),
                    ratatui::style::Style::default()
                        .fg(ratatui::style::Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ]);
            let inner = block.inner(popup);
            let [input_area, help_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
                    .areas(inner);
            frame.render_widget(ratatui::widgets::Paragraph::new(input_line), input_area);
            let help = ratatui::text::Span::styled(
                " Enter: submit  Esc: cancel ",
                ratatui::style::Style::default().fg(ratatui::style::Color::White),
            );
            frame.render_widget(ratatui::widgets::Paragraph::new(help), help_area);
            frame.render_widget(block, popup);
        }
    }
}

// ---------------------------------------------------------------------------
// Message list sub-view
// ---------------------------------------------------------------------------

fn render_message_list(frame: &mut Frame, area: Rect, state: &RenderState) {
    // When in search mode, filter by search_uids.
    let search_filter: Option<std::collections::HashSet<u32>> =
        if state.mode == Mode::Search {
            Some(state.search_uids.iter().copied().collect())
        } else {
            None
        };

    let items: Vec<ListItem<'_>> = if state.threaded && !state.thread_entries.is_empty() {
        // Build from thread entries with indentation.
        let msg_by_uid: std::collections::HashMap<u32, &Message> =
            state.mailbox.messages.iter().map(|m| (m.uid, m)).collect();

        state
            .thread_entries
            .iter()
            .map(|entry| {
                let indent = "  ".repeat(entry.depth);
                let msg = msg_by_uid.get(&entry.uid);

                let unseen = if entry.has_unseen {
                    Span::styled(" N ", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("   ")
                };

                let subject = msg.map(|m| m.envelope.subject.as_str()).unwrap_or("");
                let from = msg.map(|m| m.envelope.from.as_str()).unwrap_or("");
                let date = msg.map(|m| m.envelope.date.as_str()).unwrap_or("");

                let (sw, fw, dw) = state.column_widths;
                Line::from(vec![
                    Span::raw(indent),
                    unseen,
                    Span::raw(" "),
                    Span::raw(truncate_str(subject, sw)),
                    Span::raw("  "),
                    Span::raw(truncate_str(from, fw)),
                    Span::raw("  "),
                    Span::raw(truncate_str(date, dw)),
                ])
                .into()
            })
            .collect()
    } else {
        // Flat list — optionally filtered by search.
        state
            .mailbox
            .messages
            .iter()
            .filter(|msg| {
                search_filter
                    .as_ref()
                    .map(|f| f.contains(&msg.uid))
                    .unwrap_or(true)
            })
            .map(|msg| {
                let unseen = if msg.flags.contains(FlagSet::SEEN) {
                    Span::raw("   ")
                } else {
                    Span::styled(" N ", Style::default().fg(Color::Yellow))
                };

                let (sw, fw, dw) = state.column_widths;
                Line::from(vec![
                    unseen,
                    Span::raw(" "),
                    Span::raw(truncate_str(&msg.envelope.subject, sw)),
                    Span::raw("  "),
                    Span::raw(truncate_str(&msg.envelope.from, fw)),
                    Span::raw("  "),
                    Span::raw(truncate_str(&msg.envelope.date, dw)),
                ])
                .into()
            })
            .collect()
    };

    let view_mode = if state.threaded { "threaded" } else { "flat" };
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(
                    " Messages — {} [{}]",
                    state.mailbox_name, view_mode
                ))
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut offset_state(state.selected_index));
}

fn render_status_bar(frame: &mut Frame, area: Rect, state: &RenderState) {
    let total = state.mailbox.messages.len();
    let unseen = state.mailbox.unseen_count();

    let mut left = format!(
        " {} — {} unread, {} total ",
        state.mailbox_name, unseen, total
    );
    if state.new_mail_count > 0 {
        left.push_str(&format!("⚡{} new ", state.new_mail_count));
    }
    let right = " c:compose  r:reply  t:thread  [ ]:acct  ↑↓:nav  q:quit  ".to_owned();

    let padding = area
        .width
        .saturating_sub(left.len() as u16 + right.len() as u16)
        .max(1) as usize;

    let bar = Line::from(vec![
        Span::styled(
            left.clone(),
            Style::default().bg(Color::DarkGray).fg(Color::White),
        ),
        Span::styled(" ".repeat(padding), Style::default().bg(Color::DarkGray)),
        Span::styled(right, Style::default().bg(Color::DarkGray).fg(Color::White)),
    ]);

    frame.render_widget(Paragraph::new(bar), area);
}

// ---------------------------------------------------------------------------
// Compose sub-view
// ---------------------------------------------------------------------------

fn render_compose(frame: &mut Frame, area: Rect, state: &RenderState) {
    let c = &state.compose;

    // Determine which field is active for highlighting.
    let (to_style, subj_style, body_style) = field_styles(c.active_field);

    let to_line = Line::from(vec![
        Span::styled(" To:     ", to_style),
        Span::styled(format!("{}▎", c.to), to_style),
    ]);
    let subj_line = Line::from(vec![
        Span::styled(" Subject:", subj_style),
        Span::styled(format!("{}▎", c.subject), subj_style),
    ]);

    let title = if c.in_reply_to.is_some() {
        " Compose — Reply "
    } else {
        " Compose — New Message "
    };

    let block = Block::default().title(title).borders(Borders::ALL);

    // Layout: To (1 line), Subject (1 line), separator, Body (remaining)
    let inner = block.inner(area);
    let [to_area, subj_area, sep_area, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new(to_line), to_area);
    frame.render_widget(Paragraph::new(subj_line), subj_area);

    // Autocomplete dropdown for To field.
    if !state.contacts.is_empty() && c.active_field == ComposeField::To {
        let drop_lines: Vec<Line<'_>> = state
            .contacts
            .iter()
            .enumerate()
            .map(|(i, contact)| {
                let prefix = if i == 0 { "▶ " } else { "  " };
                Line::from(format!(
                    "{prefix}{} <{}>",
                    contact.name, contact.email
                ))
            })
            .collect();
        if !drop_lines.is_empty() {
            let h = drop_lines.len() as u16;
            let drop_area = Rect {
                y: to_area.y + 1,
                height: h.min(5),
                ..to_area
            };
            let drop = Paragraph::new(drop_lines).block(
                Block::default().style(Style::default().bg(Color::DarkGray)),
            );
            frame.render_widget(drop, drop_area);
        }
    }
    frame.render_widget(
        Paragraph::new("─".repeat(inner.width as usize)),
        sep_area,
    );

    // Body — show cursor indicator at the end of the text.
    let body_text = format!("{}▎", c.body);
    let body_para = Paragraph::new(body_text).style(body_style);
    frame.render_widget(body_para, body_area);

    frame.render_widget(block, area);

    // Attached files list.
    if !state.attached_files.is_empty() {
        let names: Vec<String> = state
            .attached_files
            .iter()
            .map(|f| format!("📎 {} ({} bytes)", f.name, f.size))
            .collect();
        let att_line = format!("Attachments: {}", names.join(", "));
        let att_y = area.y + area.height.saturating_sub(2);
        let att_area = Rect {
            y: att_y,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Span::styled(att_line, Style::default().fg(Color::Cyan))),
            att_area,
        );
    }

    // Show status if present.
    if let Some(ref msg) = state.status_message {
        let status = Paragraph::new(Span::styled(
            msg.clone(),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        let status_area = Rect {
            y: area.y + area.height.saturating_sub(2),
            height: 1,
            ..area
        };
        frame.render_widget(status, status_area);
    }
}

fn render_search_bar(frame: &mut Frame, area: Rect, state: &RenderState) {
    let prompt = format!("/{}▎", state.search_query);
    let bar = Paragraph::new(Span::styled(
        prompt,
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(bar, area);
}

// ---------------------------------------------------------------------------
// Detail view
// ---------------------------------------------------------------------------

fn render_detail_view(frame: &mut Frame, area: Rect, state: &RenderState) {
    let sel_uid = state.selected_uid();
    let msg = state
        .mailbox
        .messages
        .iter()
        .find(|m| sel_uid == Some(m.uid));

    let Some(msg) = msg else {
        let p = Paragraph::new("No message selected.");
        frame.render_widget(p, area);
        return;
    };

    // Layout: headers (4 lines), separator, body, attachments line
    let header_text = format!(
        "From: {}\nTo: {}\nDate: {}\nSubject: {}",
        msg.envelope.from,
        msg.envelope.to,
        msg.envelope.date,
        msg.envelope.subject
    );
    let attach_text = if msg.attachments.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = msg
            .attachments
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let marker = if i == state.detail_attach_index {
                    "▶ "
                } else {
                    "  "
                };
                format!(
                    "{marker}{} ({}, {} bytes)",
                    a.filename, a.content_type, a.size
                )
            })
            .collect();
        parts.join("\n")
    };

    let pgp_text = if msg.body.contains("-----BEGIN PGP") {
        let detected = neomutt_pgp::detect(msg.body.as_bytes());
        match detected {
            neomutt_pgp::PgpContent::Encrypted => "🔒 Encrypted".to_owned(),
            neomutt_pgp::PgpContent::Signed => "✓ Signed".to_owned(),
            _ => String::new(),
        }
    } else {
        String::new()
    };

    let attach_lines = if attach_text.is_empty() {
        0
    } else {
        attach_text.lines().count() as u16
    };
    let pgp_line = if pgp_text.is_empty() { 0 } else { 1 };

    let [header_area, sep_area, body_area, att_area, pgp_area] =
        Layout::vertical([
            Constraint::Length(4),              // headers
            Constraint::Length(1),              // separator
            Constraint::Min(0),                 // body
            Constraint::Length(attach_lines),   // attachments (0 if none)
            Constraint::Length(pgp_line),       // PGP status (0 if none)
        ])
        .areas(area);

    // Headers
    frame.render_widget(
        Paragraph::new(header_text)
            .block(Block::default().borders(Borders::ALL).title(" Message ")),
        header_area,
    );

    // Separator
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)),
        sep_area,
    );

    // Body — scrollable
    let body_text = if msg.body.is_empty() && !msg.body_fetched {
        "[body not fetched]".to_owned()
    } else if msg.body.is_empty() {
        "[no body text]".to_owned()
    } else {
        msg.body.clone()
    };
    let body_lines: Vec<&str> = body_text.lines().collect();
    let scroll_max =
        body_lines.len().saturating_sub(body_area.height as usize);
    let scroll = state.detail_scroll.min(scroll_max);
    let visible: String = body_lines
        .iter()
        .skip(scroll)
        .take(body_area.height as usize)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(Paragraph::new(visible), body_area);

    // Attachments
    if attach_lines > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                &attach_text,
                Style::default().fg(Color::Cyan),
            )),
            att_area,
        );
    }
    // PGP status
    if pgp_line > 0 {
        let color = if pgp_text.starts_with('🔒') {
            Color::Yellow
        } else {
            Color::Green
        };
        frame.render_widget(
            Paragraph::new(Span::styled(&pgp_text, Style::default().fg(color))),
            pgp_area,
        );
    }
}

fn render_detail_help(frame: &mut Frame, area: Rect) {
    let left = " ↑↓/jk:scroll  Tab:next att  s:save att  Esc/q:back to list  ";
    let bar = Span::styled(
        left,
        Style::default().bg(Color::DarkGray).fg(Color::White),
    );
    frame.render_widget(Paragraph::new(bar), area);
}

fn render_compose_help(frame: &mut Frame, area: Rect, state: &RenderState) {
    let sig = if state.compose.sign { "SIG" } else { "sig" };
    let enc = if state.compose.encrypt { "ENC" } else { "enc" };
    let left = format!(
        " Ctrl+X:send  Tab:next  Enter:newline(body)  BS:delete  ^S:{sig}  ^E:{enc}  Esc:cancel "
    );
    let right = match state.status_message {
        Some(ref msg) => format!("  {msg}  "),
        None => String::new(),
    };

    let padding = area
        .width
        .saturating_sub(left.len() as u16 + right.len() as u16)
        .max(1) as usize;

    let bar = Line::from(vec![
        Span::styled(left, Style::default().bg(Color::DarkGray).fg(Color::White)),
        Span::styled(" ".repeat(padding), Style::default().bg(Color::DarkGray)),
        Span::styled(right, Style::default().bg(Color::DarkGray).fg(Color::Yellow)),
    ]);

    frame.render_widget(Paragraph::new(bar), area);
}

fn field_styles(active: ComposeField) -> (Style, Style, Style) {
    let active_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default();

    match active {
        ComposeField::To => (active_style, inactive_style, inactive_style),
        ComposeField::Subject => (inactive_style, active_style, inactive_style),
        ComposeField::Body => (inactive_style, inactive_style, active_style),
    }
}

// ---------------------------------------------------------------------------
// Terminal event loop
// ---------------------------------------------------------------------------

/// Initialise the terminal, enter the alternate screen, and run the UI event
/// loop.
pub fn run(
    mut rx_state: mpsc::Receiver<RenderState>,
    tx_commands: mpsc::Sender<Command>,
) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut rx_state, &tx_commands);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    rx_state: &mut mpsc::Receiver<RenderState>,
    tx_commands: &mpsc::Sender<Command>,
) -> io::Result<()> {
    let mut current: Option<RenderState> = None;
    /// Local buffer for passphrase entry — never sent through RenderState.
    let mut passphrase_buf = String::new();

    loop {
        while let Ok(state) = rx_state.try_recv() {
            // If we just switched INTO PassphrasePrompt, clear the buffer.
            if state.mode == Mode::PassphrasePrompt
                && current.as_ref().map(|c| &c.mode) != Some(&Mode::PassphrasePrompt)
            {
                passphrase_buf.clear();
            }
            // Update masked length for rendering.
            if let Some(ref mut s) = current {
                // carry over from previous
            }
            current = Some(state);
        }

        if let Some(ref mut state) = current {
            // Reflect current typed count in the render snapshot.
            if state.mode == Mode::PassphrasePrompt {
                state.passphrase_masked_len = passphrase_buf.len();
            }
            terminal.draw(|frame| render(frame, state))?;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        let ev = event::read()?;

        // Handle passphrase input locally — never goes through RenderState.
        if current.as_ref().is_some_and(|s| s.mode == Mode::PassphrasePrompt) {
            match &ev {
                Event::Key(key) if key.code == KeyCode::Esc => {
                    passphrase_buf.clear();
                    let _ = tx_commands.blocking_send(Command::PassphraseCancel);
                    continue;
                }
                Event::Key(key) if key.code == KeyCode::Enter => {
                    let pw = std::mem::take(&mut passphrase_buf);
                    let _ = tx_commands.blocking_send(Command::PassphraseSubmit(pw));
                    continue;
                }
                Event::Key(key) if key.code == KeyCode::Backspace => {
                    passphrase_buf.pop();
                    continue;
                }
                Event::Key(key) => {
                    if let KeyCode::Char(ch) = key.code {
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT)
                        {
                            passphrase_buf.push(ch);
                        }
                    }
                    continue;
                }
                _ => continue,
            }
        }

        let cmd = key_to_command(&ev, current.as_ref());
        if let Some(cmd) = cmd
            && tx_commands.blocking_send(cmd).is_err() {
                return Ok(());
            }
    }
}

fn key_to_command(event: &Event, state: Option<&RenderState>) -> Option<Command> {
    match event {
        Event::Key(key) => {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let mode = state.map(|s| &s.mode);

            // Check custom keybindings from config first.
            let combo = key_combo_string(key);
            if let Some(cmd_name) = state
                .and_then(|s| s.keybindings.get(&combo))
                && let Some(cmd) = command_from_name(cmd_name, state) {
                    return Some(cmd);
                }

            // Sidebar navigation overrides message-list keys when sidebar is open.
            if state.is_some_and(|s| s.show_mailbox_list) {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => return Some(Command::Quit),
                    KeyCode::Up | KeyCode::Char('k') => return Some(Command::NavigateUp),
                    KeyCode::Down | KeyCode::Char('j') => return Some(Command::NavigateDown),
                    KeyCode::Enter => {
                        let mb = state
                            .and_then(|s| s.mailboxes.get(s.mailbox_list_index).cloned())?;
                        return Some(Command::SelectMailbox(mb.name));
                    }
                    KeyCode::Char('b') => return Some(Command::ToggleMailboxList),
                    // Delete confirmation overrides all else.
                    _ if state.is_some_and(|s| s.delete_confirm.is_some()) => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            return Some(Command::DeleteMailboxConfirm);
                        }
                        _ => return Some(Command::CreateMailbox(String::new())), // clear confirm
                    },
                    // Create mode — typing.
                    _ if state.is_some_and(|s| s.mode == Mode::MailboxCreate) => match key.code {
                        KeyCode::Esc => return Some(Command::ToggleMailboxList),
                        KeyCode::Enter => {
                            let name = state.map(|s| s.mailbox_create_input.clone())?;
                            return Some(Command::CreateMailboxConfirm(name));
                        }
                        KeyCode::Backspace => {
                            return Some(Command::CreateMailbox(
                                state.map(|s| {
                                    let mut n = s.mailbox_create_input.clone();
                                    n.pop();
                                    n
                                }).unwrap_or_default(),
                            ));
                        }
                        KeyCode::Char(ch) => {
                            let mut n = state.map(|s| s.mailbox_create_input.clone()).unwrap_or_default();
                            n.push(ch);
                            return Some(Command::CreateMailbox(n));
                        }
                        _ => {}
                    },
                    KeyCode::Char('n') => return Some(Command::CreateMailbox(String::new())),
                    KeyCode::Char('D') => {
                        let mb = state
                            .and_then(|s| s.mailboxes.get(s.mailbox_list_index).cloned())?;
                        return Some(Command::DeleteMailbox(mb.name));
                    }
                    _ => {}
                }
            }

            match mode {
                // -- message list key bindings ---------------------------------
                Some(Mode::MessageList) | Some(Mode::MailboxCreate) | Some(Mode::PassphrasePrompt) | None => match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => Some(Command::Quit),
                    KeyCode::Up | KeyCode::Char('k') => Some(Command::NavigateUp),
                    KeyCode::Down | KeyCode::Char('j') => Some(Command::NavigateDown),
                    KeyCode::Char('c') => Some(Command::OpenCompose),
                    KeyCode::Char('r') => Some(Command::OpenReply),
                    KeyCode::Char('R') | KeyCode::Char('a') => Some(Command::OpenReplyAll),
                    KeyCode::Char('t') => Some(Command::ToggleThreaded),
                    KeyCode::Char('/') => Some(Command::OpenSearch),
                    KeyCode::Enter => {
                        state.and_then(|s| selected_uid(s).map(Command::OpenMessage))
                    }
                    KeyCode::Char('m') => {
                        state.and_then(|s| selected_uid(s).map(Command::ToggleSeen))
                    }
                    KeyCode::Char('*') | KeyCode::Char('s') => {
                        state.and_then(|s| selected_uid(s).map(Command::ToggleFlagged))
                    }
                    KeyCode::Char('d') => {
                        state.and_then(|s| selected_uid(s).map(Command::Delete))
                    }
                    KeyCode::Char('$') => Some(Command::Expunge),
                    KeyCode::Char('C') => {
                        state.and_then(|s| selected_uid(s).map(Command::CopyMessage))
                    }
                    KeyCode::Char('M') => {
                        state.and_then(|s| selected_uid(s).map(Command::MoveMessage))
                    }
                    KeyCode::Char('b') => Some(Command::ToggleMailboxList),
                    KeyCode::Char(']') => Some(Command::NextAccount),
                    KeyCode::Char('[') => Some(Command::PrevAccount),
                    _ => None,
                },

                // -- file browser key bindings ---------------------------------
                Some(Mode::FileBrowser) => match (ctrl, key.code) {
                    (_, KeyCode::Esc) => Some(Command::BrowseCancel),
                    (_, KeyCode::Up) | (_, KeyCode::Char('k')) => Some(Command::BrowseUp),
                    (_, KeyCode::Down) | (_, KeyCode::Char('j')) => Some(Command::BrowseDown),
                    (_, KeyCode::Enter) => {
                        state.and_then(|s| {
                            let entry = s.browser_files.get(s.browser_index)?;
                            if entry.is_dir {
                                Some(Command::BrowseDir(entry.name.clone()))
                            } else {
                                let path = format!("{}/{}", s.browser_path, entry.name);
                                Some(Command::BrowseSelect(path))
                            }
                        })
                    }
                    _ => None,
                },

                // -- detail view key bindings ----------------------------------
                Some(Mode::MessageDetail) => match (ctrl, key.code) {
                    (_, KeyCode::Esc)
                    | (_, KeyCode::Char('q'))
                    | (_, KeyCode::Char('Q')) => Some(Command::CloseDetail),
                    (_, KeyCode::Up) | (_, KeyCode::Char('k')) => {
                        Some(Command::DetailScrollUp)
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
                        Some(Command::DetailScrollDown)
                    }
                    (_, KeyCode::Tab) => Some(Command::DetailAttachNext),
                    (_, KeyCode::Char('s')) => Some(Command::SaveAttachment),
                    (_, KeyCode::Char('H')) => Some(Command::OpenInBrowser),
                    _ => None,
                },

                // -- search mode key bindings ----------------------------------
                Some(Mode::Search) => match (ctrl, key.code) {
                    (_, KeyCode::Esc) => Some(Command::CancelSearch),
                    (_, KeyCode::Enter) => Some(Command::RunSearch),
                    (_, KeyCode::Backspace) => Some(Command::SearchBackspace),
                    (false, KeyCode::Char(ch)) => Some(Command::SearchInput(ch)),
                    _ => None,
                },

                // -- compose mode key bindings ----------------------------------
                Some(Mode::Compose) => match (ctrl, key.code) {
                    // Send (Ctrl+X)
                    (true, KeyCode::Char('x')) | (true, KeyCode::Char('X')) => {
                        Some(Command::SendCompose)
                    }
                    // Save draft (Ctrl+D)
                    (true, KeyCode::Char('d')) | (true, KeyCode::Char('D')) => {
                        Some(Command::SaveDraft)
                    }
                    // File browser
                    (false, KeyCode::Char('a')) => Some(Command::OpenFileBrowser),
                    // Cancel
                    (_, KeyCode::Esc) => Some(Command::CancelCompose),
                    // Sign/encrypt toggles
                    (true, KeyCode::Char('s')) | (true, KeyCode::Char('S')) => {
                        Some(Command::ToggleSign)
                    }
                    (true, KeyCode::Char('e')) | (true, KeyCode::Char('E')) => {
                        Some(Command::ToggleEncrypt)
                    }
                    // Next field
                    (_, KeyCode::Tab) => Some(Command::ComposeNextField),
                    // Newline (in Body it's literal, in To/Subject it acts as next field)
                    (_, KeyCode::Enter) => Some(Command::ComposeNewline),
                    // Backspace
                    (_, KeyCode::Backspace) => Some(Command::ComposeBackspace),
                    // Printable chars
                    (false, KeyCode::Char(ch)) => Some(Command::ComposeInput(ch)),
                    _ => None,
                },

            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl RenderState {
    /// Return the UID of the currently selected message (handles both
    /// flat and threaded views).
    pub fn selected_uid(&self) -> Option<u32> {
        if self.threaded && !self.thread_entries.is_empty() {
            self.thread_entries
                .get(self.selected_index)
                .map(|e| e.uid)
        } else {
            self.mailbox
                .messages
                .get(self.selected_index)
                .map(|m| m.uid)
        }
    }
}

fn selected_uid(state: &RenderState) -> Option<u32> {
    state.selected_uid()
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        format!("{:<width$}", s, width = max_chars)
    } else {
        format!(
            "{}…",
            &s.chars().take(max_chars.saturating_sub(1)).collect::<String>()
        )
    }
}

fn render_mailbox_sidebar(
    frame: &mut Frame,
    area: Rect,
    state: &RenderState,
) {
    let _ = &state.delete_confirm; // read in condition below
    let title = if state.delete_confirm.is_some() {
        let name = state.delete_confirm.as_deref().unwrap_or("");
        format!(" Delete '{name}'? y/n ")
    } else if state.mode == Mode::MailboxCreate {
        format!(" New mailbox: {}▎ ", state.mailbox_create_input)
    } else {
        " Mailboxes ".to_owned()
    };
    let items: Vec<ListItem<'_>> = state
        .mailboxes
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let prefix = if i == state.mailbox_list_index { "▶ " } else { "  " };
            Line::from(format!("{prefix}{}", entry.label)).into()
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut file_browser_state(state.mailbox_list_index));
}

fn file_browser_state(index: usize) -> ratatui::widgets::ListState {
    let mut s = ratatui::widgets::ListState::default();
    s.select(Some(index));
    s
}

fn key_combo_string(key: &crossterm::event::KeyEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl".into());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("Alt".into());
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("Shift".into());
    }
    let ch = match key.code {
        KeyCode::Char(c) => c.to_uppercase().to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        other => format!("{other:?}"),
    };
    parts.push(ch);
    parts.join("+")
}

fn command_from_name(name: &str, state: Option<&RenderState>) -> Option<Command> {
    match name {
        "Quit" => Some(Command::Quit),
        "NavigateUp" => Some(Command::NavigateUp),
        "NavigateDown" => Some(Command::NavigateDown),
        "OpenCompose" => Some(Command::OpenCompose),
        "OpenReply" => Some(Command::OpenReply),
        "OpenReplyAll" => Some(Command::OpenReplyAll),
        "OpenSearch" => Some(Command::OpenSearch),
        "RunSearch" => Some(Command::RunSearch),
        "CancelSearch" => Some(Command::CancelSearch),
        "CancelCompose" => Some(Command::CancelCompose),
        "SendCompose" => Some(Command::SendCompose),
        "SaveDraft" => Some(Command::SaveDraft),
        "ToggleSign" => Some(Command::ToggleSign),
        "ToggleEncrypt" => Some(Command::ToggleEncrypt),
        "ToggleThreaded" => Some(Command::ToggleThreaded),
        "NextAccount" => Some(Command::NextAccount),
        "PrevAccount" => Some(Command::PrevAccount),
        "ToggleMailboxList" => Some(Command::ToggleMailboxList),
        "Expunge" => Some(Command::Expunge),
        "CloseDetail" => Some(Command::CloseDetail),
        "DetailScrollUp" => Some(Command::DetailScrollUp),
        "DetailScrollDown" => Some(Command::DetailScrollDown),
        "DetailAttachNext" => Some(Command::DetailAttachNext),
        "SaveAttachment" => Some(Command::SaveAttachment),
        "OpenInBrowser" => Some(Command::OpenInBrowser),
        "OpenFileBrowser" => Some(Command::OpenFileBrowser),
        "BrowseCancel" => Some(Command::BrowseCancel),
        "BrowseUp" => Some(Command::BrowseUp),
        "BrowseDown" => Some(Command::BrowseDown),
        "ToggleSeen" | "Delete" | "ToggleFlagged" | "CopyMessage" | "MoveMessage"
            if state.and_then(selected_uid).is_some() =>
        {
            // Defensive: the guard ensures this is Some, but use `if let`
            // instead of `.unwrap()` so a refactoring mistake can't cause
            // a panic — it silently produces no command instead.
            if let Some(uid) = state.and_then(selected_uid) {
                match name {
                    "ToggleSeen" => Some(Command::ToggleSeen(uid)),
                    "ToggleFlagged" => Some(Command::ToggleFlagged(uid)),
                    "Delete" => Some(Command::Delete(uid)),
                    "CopyMessage" => Some(Command::CopyMessage(uid)),
                    "MoveMessage" => Some(Command::MoveMessage(uid)),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn offset_state(selected_index: usize) -> ratatui::widgets::ListState {
    let mut s = ratatui::widgets::ListState::default();
    s.select(Some(selected_index));
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_subject_adds_re_prefix() {
        assert_eq!(reply_subject("Hello"), "Re: Hello");
    }

    #[test]
    fn reply_subject_avoids_double_prefix() {
        assert_eq!(reply_subject("Re: Hello"), "Re: Hello");
        assert_eq!(reply_subject("RE: Hello"), "RE: Hello");
        assert_eq!(reply_subject("re: Hello"), "re: Hello");
    }

    #[test]
    fn reply_compose_state_prefills_fields() {
        let original = neomutt_core::Message::new(
            1,
            neomutt_core::Envelope {
                subject: "Test".into(),
                from: "alice@example.com".into(),
                to: "bob@example.com".into(),
                date: "2024-01-01".into(),
                message_id: "<msg-1@ex>".into(),
                in_reply_to: String::new(),
                references: String::new(),
            },
            neomutt_core::FlagSet::default(),
        );

        let cs = reply_compose_state(&original, false);
        assert_eq!(cs.to, "alice@example.com");
        assert_eq!(cs.subject, "Re: Test");
        assert!(cs.body.contains("2024-01-01"));
        assert!(cs.body.contains("alice@example.com"));
        assert_eq!(cs.in_reply_to.as_deref(), Some("<msg-1@ex>"));
        assert_eq!(cs.references.as_deref(), Some("<msg-1@ex>"));
        assert_eq!(cs.active_field, ComposeField::Body);
    }

    #[test]
    fn reply_all_includes_all_recipients() {
        let original = neomutt_core::Message::new(
            1,
            neomutt_core::Envelope {
                subject: "Group thread".into(),
                from: "alice@a.com".into(),
                to: "bob@b.com, carol@c.com".into(),
                date: "now".into(),
                message_id: "<g1>".into(),
                in_reply_to: String::new(),
                references: String::new(),
            },
            neomutt_core::FlagSet::default(),
        );

        let cs = reply_compose_state(&original, true);
        assert!(cs.to.contains("alice@a.com"));
        assert!(cs.to.contains("bob@b.com"));
        assert!(cs.to.contains("carol@c.com"));
    }

    #[test]
    fn compose_field_next_cycles() {
        assert_eq!(ComposeField::To.next(), ComposeField::Subject);
        assert_eq!(ComposeField::Subject.next(), ComposeField::Body);
        assert_eq!(ComposeField::Body.next(), ComposeField::To);
    }

    #[test]
    fn compose_state_default_is_to_field() {
        let cs = ComposeState::default();
        assert_eq!(cs.active_field, ComposeField::To);
        assert!(cs.to.is_empty());
        assert!(cs.subject.is_empty());
        assert!(cs.body.is_empty());
        assert!(cs.in_reply_to.is_none());
    }
}
