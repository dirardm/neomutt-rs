//! neomutt-rs — terminal email client (runtime entrypoint).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use neomutt::{
    apply_command, apply_event, build_draft_message, build_render, sanitize_html,
    save_attachment_to_disk, send_via_smtp, take_attach_save_request, take_copy_move_action,
    take_send_request, take_search_request, AccountState, AppState, Args, CommandOutcome,
};
use neomutt_cache::MailboxCache;
use neomutt_config::load_config;
use neomutt_core::FlagSet;
use neomutt_mail_store::{append_message_async, copy_message_async, create_mailbox_async, delete_mailbox_async, expunge_async, idle_loop, move_message_async, set_flags_async, ImapClient, ImapConfig, ImapEvent};
use neomutt_pgp::Keyring;
use neomutt_search::SearchIndex;
use neomutt_ui::{Command, ComposeState, Mode, RenderState};

// ---------------------------------------------------------------------------
// Channel helpers — bounded channels with rate-limited overflow logging
// ---------------------------------------------------------------------------

/// Rate-limited counter so we don't spam stderr on every overflow event.
static CHANNEL_FULL_COUNT: AtomicU64 = AtomicU64::new(0);

fn log_channel_full(name: &str) {
    let n = CHANNEL_FULL_COUNT.fetch_add(1, Ordering::Relaxed);
    // Log every 100th event — enough to notice, not enough to flood.
    if n % 100 == 0 {
        log::warn!("[channel] {name} full (dropped ~{n} events)");
    }
}

/// `try_send` that drops new frames when the channel is full.
/// Used for render-state channels where only the latest value matters
/// — but since the sender can't drain, we simply discard new frames
/// when the UI is behind (the event loop pushes another frame on the
/// next iteration anyway).
fn try_send_coalesce<T>(tx: &mpsc::Sender<T>, value: T, name: &str) {
    match tx.try_send(value) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            log_channel_full(name);
            // Drop: the UI already has 2 frames queued; it'll catch up
            // and the event loop will push the next frame.
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

#[tokio::main]
async fn main() {
    // Initialise structured logging.  Level is filterable at runtime via
    // RUST_LOG env var (error, warn, info, debug).  Defaults to `info`.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args = Args::init();

    // -- load accounts -------------------------------------------------------
    let (account_configs, notif_config, download_config, imap_timeouts, search_config, keybindings, display_config) =
        load_config().expect("failed to load config");
    let text_wrap_width = display_config.text_wrap_width;

    // -- clean up old temp HTML files from previous sessions ---------------
    if let Ok(dir) = std::fs::read_dir(&download_config.html_temp_dir) {
        for entry in dir.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let mailbox_name =
        std::env::var("IMAP_MAILBOX").unwrap_or_else(|_| "INBOX".to_owned());

    // -- local cache ---------------------------------------------------------
    let cache_path = std::env::var("NEOMUTT_CACHE")
        .unwrap_or_else(|_| "neomutt.db".to_owned());
    let cache = MailboxCache::open_with_limits(
        &cache_path,
        imap_timeouts.max_cached_messages_per_mailbox,
        imap_timeouts.max_contacts,
    )
    .expect("failed to open cache");

    // -- search index ---------------------------------------------------------
    let search_index_path =
        std::env::var("NEOMUTT_SEARCH").unwrap_or_else(|_| "neomutt-search".to_owned());
    let search_index =
        SearchIndex::open(
            std::path::Path::new(&search_index_path),
            search_config.writer_buffer_bytes,
            search_config.max_indexed_messages,
        )
        .expect("failed to open search index");

    // -- channels ------------------------------------------------------------
    // tx_events:  IMAP → event loop.  Every event matters (backpressure, 256).
    let (tx_events_raw, mut rx_events) = mpsc::channel::<ImapEvent>(256);
    let tx_events = neomutt_mail_store::ImapEventSender::new(tx_events_raw);
    // tx_commands: UI → event loop.  Every keypress matters (backpressure, 64).
    let (tx_commands, mut rx_commands) = mpsc::channel::<Command>(64);
    // tx_render: event loop → UI.  Only latest frame matters (coalesce, 2).
    let (tx_render, rx_render) = mpsc::channel::<RenderState>(2);

    // -- spawn IMAP tasks (one per account) ----------------------------------
    let mut accounts_map: HashMap<String, AccountState> = HashMap::new();
    let mut switch_senders: HashMap<String, mpsc::Sender<String>> =
        HashMap::new();
    for acct in &account_configs {
        let cached = cache
            .load_mailbox(&acct.name, &mailbox_name)
            .unwrap_or_default();
        let uv = cache.get_uid_validity(&acct.name, &mailbox_name);
        accounts_map.insert(
            acct.name.clone(),
            AccountState {
                mailbox: neomutt_core::Mailbox {
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

        let imap_cfg = ImapConfig {
            host: acct.imap_host.clone(),
            port: acct.imap_port,
            security: acct.imap_security,
            user: acct.imap_user.clone(),
            pass: acct.imap_pass.clone(),
            oauth2_token: acct.imap_oauth2_token.clone(),
            oauth2_refresh_token: String::new(),
            oauth2_client_id: String::new(),
            oauth2_client_secret: String::new(),
            oauth2_token_endpoint: String::new(),
            backoff_init_secs: imap_timeouts.backoff_init_secs,
            backoff_max_secs: imap_timeouts.backoff_max_secs,
            poll_interval_secs: imap_timeouts.poll_interval_secs,
            max_fetch_size_bytes: imap_timeouts.max_fetch_size_bytes,
        };
        let tx = tx_events.clone();
        let mb = mailbox_name.clone();
        let name = acct.name.clone();
        // tx_switch: only latest mailbox switch matters (replace, cap 1).
        let (tx_switch, rx_switch) =
            mpsc::channel::<String>(1);
        switch_senders.insert(name.clone(), tx_switch);
        tokio::spawn(async move {
            idle_loop(name, &imap_cfg, &mb, tx, rx_switch).await;
        });
    }

    let active = args
        .resolve_account(&account_configs)
        .expect("invalid --account");

    let mut state = AppState {
        accounts: accounts_map,
        active_account: active,
        mailbox_name,
        mode: Mode::MessageList,
        compose: ComposeState::default(),
        status_message: None,
        threaded: false,
        cache,
        search: search_index,
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
        switch_senders,
        pending_copy_move_action: None,
        mailbox_create_input: String::new(),
        delete_confirm: None,
        keybindings,
        display_config,
        draft_replacing_uid: None,
        passphrase_prompt: None,
        passphrase_tx: None,
        pending_send: None,
    };

    log::info!(
        "neomutt-rs — {} account(s), active={}",
        state.accounts.len(),
        state.active_account
    );

    // -- UI task -------------------------------------------------------------
    let ui_handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = neomutt_ui::run(rx_render, tx_commands) {
            log::error!("[ui] error — {e}");
        }
    });

    // Push initial frame.
    try_send_coalesce(&tx_render, build_render(&state), "tx_render");

    // -- event loop ----------------------------------------------------------
    loop {
        tokio::select! {
            Some(event) = rx_events.recv() => {
                apply_event(&mut state, event);
            }

            Some(cmd) = rx_commands.recv() => {
                // Extract UIDs for async commands before cmd is moved.
                let open_uid = match &cmd {
                    Command::OpenMessage(uid) => Some(*uid),
                    _ => None,
                };
                let flag_action: Option<(u32, FlagSet)> = match &cmd {
                    Command::ToggleSeen(uid) => Some((*uid, FlagSet::SEEN)),
                    Command::ToggleFlagged(uid) => Some((*uid, FlagSet::FLAGGED)),
                    Command::Delete(uid) => Some((*uid, FlagSet::DELETED)),
                    _ => None,
                };
                let is_expunge = matches!(&cmd, Command::Expunge);
                let is_open_browser = matches!(&cmd, Command::OpenInBrowser);
                let submitted_pw: Option<String> = match &cmd {
                    Command::PassphraseSubmit(pw) => Some(pw.clone()),
                    _ => None,
                };
                let was_cancel = matches!(&cmd, Command::PassphraseCancel);
                let outcome = apply_command(&mut state, cmd);

                // Handle passphrase prompt resolution for pending sends.
                if let Some(pw) = submitted_pw {
                    if let Some((smtp_cfg, from, compose, pgp_key_path, kr_dir, paths)) =
                        state.pending_send.take()
                    {
                        let pw = if pw.is_empty() { None } else { Some(pw) };
                        spawn_send(
                            tx_render.clone(), tx_events.clone(),
                            state.mailbox_name.clone(), state.active_account.clone(),
                            state.accounts.clone(), state.threaded,
                            smtp_cfg, from, compose, pgp_key_path, kr_dir, paths, pw,
                        );
                    }
                } else if was_cancel {
                    state.pending_send = None;
                    state.mode = Mode::Compose;
                }

                // OpenMessage — on-demand body fetch.
                if let Some(uid) = open_uid {
                    let already_fetched = state
                        .accounts
                        .get(&state.active_account)
                        .and_then(|a| a.mailbox.messages.iter().find(|m| m.uid == uid))
                        .map(|m| m.body_fetched)
                        .unwrap_or(true);
                    if !already_fetched {
                        let acc = state.active_account.clone();
                        let mb = state.mailbox_name.clone();
                        let cache_path = std::env::var("NEOMUTT_CACHE")
                            .unwrap_or_else(|_| "neomutt.db".to_owned());
                        let imap_cfg =
                            imap_config_for(&account_configs, &state.active_account, &imap_timeouts);

                        // Check local cache before hitting the network.
                        let mut cache_hit = false;
                        if let Ok(cache) = MailboxCache::open(&cache_path) {
                            if let Ok(Some((body_text, html_body))) =
                                cache.load_body(&acc, &mb, uid)
                                && !body_text.is_empty()
                            {
                                let _ = tx_events.send(ImapEvent::BodyFetched {
                                    account: acc.clone(),
                                    mailbox_name: mb.clone(),
                                    uid,
                                    body: body_text,
                                    html_body,
                                });
                                cache_hit = true;
                            }
                        }

                        if !cache_hit {
                            let tx = tx_events.clone();
                            state.status_message =
                                Some(format!("fetching body for UID {uid} …"));
                            tokio::task::spawn_blocking(move || {
                                let raw = tokio::runtime::Handle::current()
                                    .block_on(async {
                                        let mut c = ImapClient::<neomutt_mail_store::ImapStream>::connect(&imap_cfg).await
                                            .map_err(|e| format!("{e}"))?;
                                        c.fetch_body(&mb, uid).await
                                            .map_err(|e| format!("{e}"))
                                    });
                                match raw {
                                    Ok(Some(bytes)) => {
                                        let text = neomutt_core::parse_body_text_with_width(
                                            &bytes, text_wrap_width
                                        );
                                        let html = neomutt_core::parse_html_body(&bytes);
                                        if let Ok(cache) = MailboxCache::open(&cache_path) {
                                            let _ = cache.cache_body(
                                                &acc, &mb, uid, &text, html.as_deref(),
                                            );
                                        }
                                        let _ = tx.send(ImapEvent::BodyFetched {
                                            account: acc,
                                            mailbox_name: mb,
                                            uid,
                                            body: text,
                                            html_body: html,
                                        });
                                    }
                                    Ok(None) => {
                                        let _ = tx.send(ImapEvent::Error {
                                            account: acc,
                                            message: format!(
                                                "body fetch: message {uid} not found"
                                            ),
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx.send(ImapEvent::Error {
                                            account: acc,
                                            message: format!("body fetch: {e}"),
                                        });
                                    }
                                }
                            });
                        }
                    }
                }

                // Optimistic flag update + async server sync.
                if let Some((uid, flag)) = flag_action {
                    let msg = state
                        .accounts
                        .get(&state.active_account)
                        .and_then(|a| a.mailbox.messages.iter().find(|m| m.uid == uid));
                    let currently_set =
                        msg.map(|m| m.flags.contains(flag)).unwrap_or(false);
                    let add = !currently_set;

                    if let Some(acct) = state.accounts.get_mut(&state.active_account)
                        && let Some(msg) =
                            acct.mailbox.messages.iter_mut().find(|m| m.uid == uid)
                        {
                            if add {
                                msg.flags.insert(flag);
                            } else {
                                msg.flags.remove(flag);
                            }
                            if let Err(e) = state.cache.save_messages(
                                &state.active_account,
                                &state.mailbox_name,
                                std::slice::from_ref(msg),
                            ) {
                                let msg = format!("cache flag update error: {e}");
                                log::error!("{msg}");
                                let _ = tx_events.send(ImapEvent::Error {
                                    account: state.active_account.clone(),
                                    message: msg,
                                });
                            }
                        }

                    // Capture pre-update state for rollback on server sync failure.
                    let pre_add = add; // whether we added or removed
                    let imap_cfg =
                        imap_config_for(&account_configs, &state.active_account, &imap_timeouts);
                    let mb = state.mailbox_name.clone();
                    let acc = state.active_account.clone();
                    let tx = tx_events.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            set_flags_async(&imap_cfg, &mb, uid, flag, pre_add).await
                        {
                            // Server rejected the flag change — tell the event
                            // loop to revert the optimistic local update.
                            let revert_action = if pre_add { "removed" } else { "added" };
                            let _ = tx.send(ImapEvent::Error {
                                account: acc.clone(),
                                message: format!(
                                    "flag sync failed (server rejected, locally \
                                     {revert_action}): {e}"
                                ),
                            });
                            // Also try to persist the rollback to cache via a
                            // fresh connection.
                            if let Ok(mut c) = ImapClient::<neomutt_mail_store::ImapStream>::connect(&imap_cfg).await {
                                if let Ok(fr) = c.fetch_headers(&mb).await {
                                    // Fetching fresh state implicitly reverts
                                    // the optimistic update on next render.
                                    let _ = tx.send(ImapEvent::MailboxUpdated {
                                        account: acc,
                                        mailbox_name: mb,
                                        mailbox: neomutt_core::Mailbox {
                                            messages: fr.messages,
                                        },
                                        uid_validity: fr.uid_validity,
                                    });
                                }
                            }
                        }
                    });
                }

                // Handle SendCompose — requires async spawn.
                if let Some((smtp_cfg, from, compose, pgp_key_path, pgp_keyring_dir, paths)) =
                    take_send_request(&state, &account_configs)
                {
                    // If signing is requested, check if the key needs a passphrase
                    // and prompt the user if it's not already cached.
                    if compose.sign && !pgp_key_path.is_empty() {
                        let sign_key = pgp_key_path.clone();
                        // Quick check: try loading the cert to see if there's a key.
                        let needs_pw = match neomutt_pgp::load_cert(&sign_key) {
                            Ok(_cert) => {
                                // Try signing with no passphrase — if it fails
                                // with "no signing-capable secret key", the
                                // key is encrypted and needs unlocking.
                                let test_sig =
                                    neomutt_pgp::sign_unlocked(&_cert, b"", None);
                                test_sig.is_err()
                            }
                            Err(_) => false,
                        };
                        if needs_pw {
                            // Store the send data and show the passphrase prompt.
                            let (tx_pw, rx_pw) =
                                tokio::sync::oneshot::channel::<String>();
                            state.pending_send = Some((
                                smtp_cfg, from, compose.clone(),
                                pgp_key_path, pgp_keyring_dir.clone(), paths.clone(),
                            ));
                            state.passphrase_tx = Some(tx_pw);
                            state.passphrase_prompt =
                                Some("Enter PGP passphrase for signing key".into());
                            state.mode = Mode::PassphrasePrompt;
                            // The render below will show the prompt.
                            // The rx_pw receiver is stored for the next event loop
                            // iteration to pick up.
                            let _ = tx_render.send(build_render(&state));
                            continue; // skip the rest — wait for user response
                        }
                    }

                    // No passphrase needed — proceed directly.
                    spawn_send(
                        tx_render.clone(), tx_events.clone(),
                        state.mailbox_name.clone(), state.active_account.clone(),
                        state.accounts.clone(), state.threaded,
                        smtp_cfg, from, compose, pgp_key_path, pgp_keyring_dir, paths,
                        None,
                    );
                }

                // Handle search — spawn_blocking since tantivy is sync.
                if let Some((query, account, mailbox)) =
                    take_search_request(&state)
                {
                    let handle = state.search.reader_handle();
                    let tx = tx_render.clone();
                    tokio::task::spawn_blocking(move || {
                        let result =
                            handle.search(&query, Some(&account), Some(&mailbox));
                        let (uids, status) = match result {
                            Ok(r) => {
                                let u: Vec<u32> = r.into_iter()
                                    .map(|(uid, _)| uid).collect();
                                let c = u.len();
                                (u, format!("{c} results"))
                            }
                            Err(e) => {
                                (vec![], format!("search error: {e}"))
                            }
                        };
                        try_send_coalesce(&tx, build_search_result(
                            uids,
                            status,
                        ), "tx_render");
                    });
                }

                // Handle Expunge — async server command + immediate refetch.
                if is_expunge {
                    let imap_cfg =
                        imap_config_for(&account_configs, &state.active_account, &imap_timeouts);
                    let mb_name = state.mailbox_name.clone();
                    let acc = state.active_account.clone();
                    let tx = tx_events.clone();
                    state.status_message = Some("expunging …".into());
                    tokio::spawn(async move {
                        if let Err(e) = expunge_async(&imap_cfg, &mb_name).await {
                            let _ = tx.send(ImapEvent::Error {
                                account: acc.clone(),
                                message: format!("expunge failed: {e}"),
                            });
                            return;
                        }
                        // Fetch fresh headers so the UI updates immediately.
                        match ImapClient::<neomutt_mail_store::ImapStream>::connect(&imap_cfg).await {
                            Ok(mut c) => match c.fetch_headers(&mb_name).await {
                                Ok(fr) => {
                                    let _ = tx.send(ImapEvent::MailboxUpdated {
                                        account: acc,
                                        mailbox_name: mb_name.clone(),
                                        mailbox: neomutt_core::Mailbox {
                                            messages: fr.messages,
                                        },
                                        uid_validity: fr.uid_validity,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(ImapEvent::Error {
                                        account: acc,
                                        message: format!("expunge refetch failed: {e}"),
                                    });
                                }
                            },
                            Err(e) => {
                                let _ = tx.send(ImapEvent::Error {
                                    account: acc,
                                    message: format!("expunge reconnect failed: {e}"),
                                });
                            }
                        }
                    });
                }

                // Handle SaveAttachment — fetch + write to disk.
                if let Some((download_dir, attachment)) =
                    take_attach_save_request(&state)
                {
                    let tx = tx_render.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = save_attachment_to_disk(
                            &download_dir, &attachment,
                        );
                        let msg = match result {
                            Ok(path) => format!("saved: {}", path.display()),
                            Err(e) => format!("save failed: {e}"),
                        };
                        try_send_coalesce(&tx, build_search_result(
                            vec![], // no search results, just status
                            msg,
                        ), "tx_render");
                    });
                }

                // Handle OpenInBrowser — write HTML to temp file + open.
                // Handle copy/move — async server operation.
                if let Some((uid, is_move, source, dest)) =
                    take_copy_move_action(&mut state)
                {
                    let imap_cfg =
                        imap_config_for(&account_configs, &state.active_account, &imap_timeouts);
                    let tx = tx_events.clone();
                    let acc = state.active_account.clone();
                    let active_mb = state.mailbox_name.clone();
                    tokio::spawn(async move {
                        let result = if is_move {
                            move_message_async(&imap_cfg, &source, uid, &dest)
                                .await
                        } else {
                            copy_message_async(&imap_cfg, &source, uid, &dest)
                                .await
                        };
                        match result {
                            Ok(()) => {
                                // If the destination is the active mailbox,
                                // refetch to pick up the new message.
                                if dest == active_mb
                                    && let Ok(mut c) =
                                        ImapClient::<neomutt_mail_store::ImapStream>::connect(&imap_cfg).await
                                        && let Ok(fr) =
                                            c.fetch_headers(&dest).await
                                        {
                                            let _ = tx.send(
                                                ImapEvent::MailboxUpdated {
                                                    account: acc.clone(),
                                                    mailbox_name: dest.clone(),
                                                    mailbox: neomutt_core::Mailbox {
                                                        messages: fr.messages,
                                                    },
                                                    uid_validity: fr.uid_validity,
                                                },
                                            );
                                        }
                                // For move: the source mailbox will be
                                // updated on the next IDLE poll.
                                let verb = if is_move { "moved" } else { "copied" };
                                let _ = tx.send(ImapEvent::Error {
                                    account: acc,
                                    message: format!(
                                        "{verb} UID {uid} to {dest}"
                                    ),
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(ImapEvent::Error {
                                    account: acc,
                                    message: format!(
                                        "copy/move failed: {e}"
                                    ),
                                });
                            }
                        }
                    });
                }

                // Handle create/delete mailbox — async server ops.
                if state
                    .status_message
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("creating mailbox")
                {
                    let name = state.mailbox_create_input.clone();
                    if !name.is_empty() {
                        let imap_cfg = imap_config_for(
                            &account_configs,
                            &state.active_account,
                            &imap_timeouts,
                        );
                        let tx = tx_events.clone();
                        let acc = state.active_account.clone();
                        tokio::spawn(async move {
                            match create_mailbox_async(&imap_cfg, &name).await
                            {
                                Ok(()) => {
                                    // Refresh mailbox list.
                                    let _ = tx.send(
                                        ImapEvent::MailboxList {
                                            account: acc,
                                            mailboxes: vec![],
                                        },
                                    );
                                }
                                Err(e) => {
                                    let _ = tx.send(ImapEvent::Error {
                                        account: acc,
                                        message: format!(
                                            "create '{name}' failed: {e}"
                                        ),
                                    });
                                }
                            }
                        });
                    }
                }
                if state
                    .status_message
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("deleting mailbox")
                    && state.delete_confirm.is_none()
                {
                    // DeleteConfirm was just handled — extract the name.
                    let name = state
                        .status_message
                        .clone()
                        .unwrap_or_default()
                        .strip_prefix("deleting mailbox '")
                        .and_then(|s| s.strip_suffix("' …"))
                        .unwrap_or("")
                        .to_owned();
                    if !name.is_empty() {
                        let imap_cfg = imap_config_for(
                            &account_configs,
                            &state.active_account,
                            &imap_timeouts,
                        );
                        let tx = tx_events.clone();
                        let acc = state.active_account.clone();
                        tokio::spawn(async move {
                            match delete_mailbox_async(&imap_cfg, &name).await
                            {
                                Ok(()) => {
                                    let _ = tx.send(
                                        ImapEvent::MailboxList {
                                            account: acc,
                                            mailboxes: vec![],
                                        },
                                    );
                                }
                                Err(e) => {
                                    let _ = tx.send(ImapEvent::Error {
                                        account: acc,
                                        message: format!(
                                            "delete '{name}' failed: {e}"
                                        ),
                                    });
                                }
                            }
                        });
                    }
                }

                // Handle SaveDraft — async APPEND to Drafts mailbox.
                if state.status_message.as_deref() == Some("saving draft …") {
                    let acct = account_configs
                        .iter()
                        .find(|a| a.name == state.active_account)
                        .cloned();
                    if let Some(acct) = acct {
                        let imap_cfg = imap_config_for(
                            &account_configs,
                            &state.active_account,
                            &imap_timeouts,
                        );
                        let from = acct.effective_from().to_owned();
                        let raw =
                            build_draft_message(&state.compose, &from);
                        let drafts_mb = acct.drafts_mailbox.clone();
                        let replace_uid = state.draft_replacing_uid;
                        let tx = tx_events.clone();
                        let acc = state.active_account.clone();
                        let mb = state.mailbox_name.clone();
                        state.status_message =
                            Some("draft saved".into());
                        tokio::spawn(async move {
                            if let Some(old_uid) = replace_uid {
                                if let Err(e) = set_flags_async(
                                    &imap_cfg, &mb, old_uid,
                                    neomutt_core::FlagSet::DELETED,
                                    true,
                                ).await {
                                    let _ = tx.send(ImapEvent::Error {
                                        account: acc.clone(),
                                        message: format!("draft delete failed: {e}"),
                                    });
                                }
                                if let Ok(mut c) = ImapClient::<neomutt_mail_store::ImapStream>::connect(&imap_cfg).await {
                                    let tx2 = tx.clone();
                                    let acc2 = acc.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = c.expunge(&mb).await {
                                            let _ = tx2.send(ImapEvent::Error {
                                                account: acc2,
                                                message: format!("draft expunge failed: {e}"),
                                            });
                                        }
                                    });
                                }
                            }
                            if let Err(e) = append_message_async(
                                &imap_cfg, &drafts_mb, &raw,
                            ).await {
                                let _ = tx.send(ImapEvent::Error {
                                    account: acc,
                                    message: format!("save draft failed: {e}"),
                                });
                            }
                        });
                    }
                }

                if is_open_browser {
                    let sel_uid = state
                        .accounts
                        .get(&state.active_account)
                        .and_then(|a| {
                            if state.threaded && !a.thread_entries.is_empty() {
                                a.thread_entries
                                    .get(a.selected_index)
                                    .map(|e| e.uid)
                            } else {
                                a.mailbox.messages
                                    .get(a.selected_index)
                                    .map(|m| m.uid)
                            }
                        });
                    let html = sel_uid
                        .and_then(|uid| {
                            state.accounts
                                .get(&state.active_account)
                                .and_then(|a| {
                                    a.mailbox.messages.iter().find(|m| m.uid == uid)
                                })
                        })
                        .and_then(|m| m.html_body.clone());
                    if let Some(html_body) = html {
                        let load_images =
                            state.download_config.html_load_remote_images;
                        let clean = sanitize_html(&html_body, load_images);
                        let dir = std::path::Path::new(&state.download_config.html_temp_dir);
                        let _ = std::fs::create_dir_all(dir);
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let path = dir.join(format!("msg_{}_{ts}.html", sel_uid.map_or(0, |u| u)));
                        if std::fs::write(&path, &clean).is_ok() {
                            let _ = open::that(&path);
                            state.status_message =
                                Some("opened HTML in browser".into());
                        } else {
                            state.status_message =
                                Some("failed to write temp HTML file".into());
                        }
                    } else {
                        state.status_message =
                            Some("no HTML part in this message".into());
                    }
                }

                if outcome == CommandOutcome::Quit {
                    break;
                }
            }
        }

        try_send_coalesce(&tx_render, build_render(&state), "tx_render");
    }

    // -- clean shutdown ------------------------------------------------------
    drop(tx_render);
    let _ = ui_handle.await;
    log::info!("neomutt-rs exited.");
}

/// Spawn a blocking task to execute the actual send.
fn spawn_send(
    tx_render: mpsc::Sender<RenderState>,
    tx_events: neomutt_mail_store::ImapEventSender,
    mb_name: String,
    acc_name: String,
    accs: HashMap<String, AccountState>,
    th: bool,
    smtp_cfg: neomutt_smtp_client::SmtpConfig,
    from: String,
    compose: ComposeState,
    pgp_key_path: String,
    kr_dir: String,
    paths: Vec<String>,
    passphrase: Option<String>,
) {
    tokio::task::spawn_blocking(move || {
        let keyring = if kr_dir.is_empty() {
            Keyring::default()
        } else {
            match Keyring::load(std::path::Path::new(&kr_dir)) {
                Ok(kr) => kr,
                Err(e) => {
                    let _ = tx_events.send(ImapEvent::Error {
                        account: acc_name.clone(),
                        message: format!("keyring load failed: {e}"),
                    });
                    Keyring::default()
                }
            }
        };
        let result = send_via_smtp(
            &smtp_cfg, &from, &compose, &pgp_key_path, &keyring, &paths, passphrase,
        );
        let (m, c, status) = match result {
            Ok(()) => (Mode::MessageList, ComposeState::default(), "sent ✓"),
            Err(e) => (Mode::Compose, compose, &format!("send failed: {e}") as &str),
        };
        try_send_coalesce(&tx_render, build_send_result(
            &acc_name, &mb_name, &accs, &m, &c, status, th,
        ), "tx_render");
    });
}

fn build_send_result(
    active_account: &str,
    mailbox_name: &str,
    accounts: &HashMap<String, AccountState>,
    mode: &Mode,
    compose: &ComposeState,
    status: &str,
    threaded: bool,
) -> RenderState {
    // Defensive fallback: if the active account disappeared (internal bug),
    // return a minimal frame with the status message rather than panicking.
    let (mailbox, selected_index, thread_entries) =
        if let Some(a) = accounts.get(active_account) {
            (a.mailbox.clone(), a.selected_index, a.thread_entries.clone())
        } else {
            log::error!(
                "[render] internal error: account '{active_account}' not found \
                 in build_send_result"
            );
            (neomutt_core::Mailbox::new(), 0, Vec::new())
        };
    RenderState {
        mode: mode.clone(),
        mailbox_name: format!("{}/{}", active_account, mailbox_name),
        mailbox,
        selected_index,
        compose: compose.clone(),
        status_message: Some(status.into()),
        threaded,
        thread_entries,
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
        mailboxes: Vec::new(),
        mailbox_list_index: 0,
        mailbox_create_input: String::new(),
        delete_confirm: None,
        keybindings: std::collections::HashMap::new(),
        column_widths: (40, 30, 24),
        passphrase_prompt: None,
        passphrase_masked_len: 0,
    }
}

fn imap_config_for(account_configs: &[neomutt_config::Account], name: &str, timeouts: &neomutt_config::ImapTimeouts) -> ImapConfig {
    account_configs
        .iter()
        .find(|a| a.name == name)
        .map(|a| ImapConfig {
            host: a.imap_host.clone(),
            port: a.imap_port,
            security: a.imap_security,
            user: a.imap_user.clone(),
            pass: a.imap_pass.clone(),
            oauth2_token: a.imap_oauth2_token.clone(),
            oauth2_refresh_token: String::new(),
            oauth2_client_id: String::new(),
            oauth2_client_secret: String::new(),
            oauth2_token_endpoint: String::new(),
            backoff_init_secs: timeouts.backoff_init_secs,
            backoff_max_secs: timeouts.backoff_max_secs,
            poll_interval_secs: timeouts.poll_interval_secs,
            max_fetch_size_bytes: timeouts.max_fetch_size_bytes,
        })
        .unwrap_or_else(|| ImapConfig {
            host: String::new(),
            port: 993,
            security: neomutt_config::ImapSecurity::Direct,
            user: String::new(),
            pass: String::new(),
            oauth2_token: String::new(),
            oauth2_refresh_token: String::new(),
            oauth2_client_id: String::new(),
            oauth2_client_secret: String::new(),
            oauth2_token_endpoint: String::new(),
            backoff_init_secs: 1,
            backoff_max_secs: 30,
            poll_interval_secs: 30,
            max_fetch_size_bytes: 25 * 1024 * 1024,
        })
}

fn build_search_result(uids: Vec<u32>, status: String) -> RenderState {
    RenderState {
        mode: Mode::Search,
        mailbox_name: String::new(),
        mailbox: neomutt_core::Mailbox::new(),
        selected_index: 0,
        compose: ComposeState::default(),
        status_message: Some(status),
        threaded: false,
        thread_entries: Vec::new(),
        search_query: String::new(),
        search_uids: uids,
        contacts: Vec::new(),
        new_mail_count: 0,
        detail_scroll: 0,
        detail_attach_index: 0,
        browser_path: String::new(),
        browser_files: Vec::new(),
        browser_index: 0,
        attached_files: Vec::new(),
        show_mailbox_list: false,
        mailboxes: Vec::new(),
        mailbox_list_index: 0,
        mailbox_create_input: String::new(),
        delete_confirm: None,
        keybindings: std::collections::HashMap::new(),
        column_widths: (40, 30, 24),
            passphrase_prompt: None,
        passphrase_masked_len: 0,
}
}
