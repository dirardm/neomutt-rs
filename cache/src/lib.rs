//! Local SQLite cache for neomutt-rs mailbox state.
//!
//! Messages are keyed by `(account, mailbox_name, uid)` so multiple
//! accounts can share one database without collisions.

use rusqlite::{params, Connection, Result as SqlResult};

use neomutt_core::{Envelope, FlagSet, Message};

// ---------------------------------------------------------------------------
// Contact
// ---------------------------------------------------------------------------

/// A learned or manually-added contact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub email: String,
}

// ---------------------------------------------------------------------------
// MailboxCache
// ---------------------------------------------------------------------------

/// Persistent cache backed by a local SQLite database.
pub struct MailboxCache {
    conn: Connection,
    max_cached_messages_per_mailbox: usize,
    max_contacts: usize,
}

impl MailboxCache {
    /// Open (or create) the cache at `path`, running any pending migrations.
    pub fn open(path: &str) -> SqlResult<Self> {
        Self::open_with_limits(path, 10_000, 5_000)
    }

    /// Open with explicit limits for message and contact storage.
    pub fn open_with_limits(
        path: &str,
        max_cached_messages_per_mailbox: usize,
        max_contacts: usize,
    ) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        run_migrations(&conn)?;
        Ok(Self {
            conn,
            max_cached_messages_per_mailbox,
            max_contacts,
        })
    }

    // -- UIDVALIDITY --------------------------------------------------------

    pub fn get_uid_validity(&self, account: &str, mailbox: &str) -> Option<u32> {
        self.conn
            .query_row(
                "SELECT validity FROM uid_validity
                 WHERE account_name = ?1 AND mailbox_name = ?2",
                params![account, mailbox],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set_uid_validity(
        &self,
        account: &str,
        mailbox: &str,
        validity: u32,
    ) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO uid_validity (account_name, mailbox_name, validity)
             VALUES (?1, ?2, ?3)",
            params![account, mailbox, validity],
        )?;
        Ok(())
    }

    // -- messages -----------------------------------------------------------

    pub fn load_mailbox(&self, account: &str, mailbox: &str) -> SqlResult<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, subject, \"from\", \"to\", date, message_id,
                    in_reply_to, refs, flags, body_fetched
             FROM messages
             WHERE account_name = ?1 AND mailbox_name = ?2
             ORDER BY uid",
        )?;

        let rows = stmt.query_map(params![account, mailbox], |row| {
            let flags_raw: u8 = row.get(8)?;
            Ok(Message {
                attachments: Vec::new(),
                body: String::new(),
                html_body: None,
                uid: row.get(0)?,
                envelope: Envelope {
                    subject: row.get::<_, String>(1).unwrap_or_default(),
                    from: row.get::<_, String>(2).unwrap_or_default(),
                    to: row.get::<_, String>(3).unwrap_or_default(),
                    date: row.get::<_, String>(4).unwrap_or_default(),
                    message_id: row.get::<_, String>(5).unwrap_or_default(),
                    in_reply_to: row.get::<_, String>(6).unwrap_or_default(),
                    references: row.get::<_, String>(7).unwrap_or_default(),
                },
                flags: FlagSet::from_bits_truncate(flags_raw),
                body_fetched: row.get(9)?,
            })
        })?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }

    pub fn save_messages(
        &self,
        account: &str,
        mailbox: &str,
        messages: &[Message],
    ) -> SqlResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO messages
                    (account_name, mailbox_name, uid, subject, \"from\", \"to\",
                     date, message_id, in_reply_to, refs, flags, body_fetched)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?;
            for msg in messages {
                stmt.execute(params![
                    account,
                    mailbox,
                    msg.uid,
                    msg.envelope.subject,
                    msg.envelope.from,
                    msg.envelope.to,
                    msg.envelope.date,
                    msg.envelope.message_id,
                    msg.envelope.in_reply_to,
                    msg.envelope.references,
                    msg.flags.bits(),
                    msg.body_fetched as u8,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn replace_mailbox(
        &self,
        account: &str,
        mailbox: &str,
        messages: &[Message],
    ) -> SqlResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE account_name = ?1 AND mailbox_name = ?2",
            params![account, mailbox],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO messages
                    (account_name, mailbox_name, uid, subject, \"from\", \"to\",
                     date, message_id, in_reply_to, refs, flags, body_fetched)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?;
            for msg in messages {
                stmt.execute(params![
                    account,
                    mailbox,
                    msg.uid,
                    msg.envelope.subject,
                    msg.envelope.from,
                    msg.envelope.to,
                    msg.envelope.date,
                    msg.envelope.message_id,
                    msg.envelope.in_reply_to,
                    msg.envelope.references,
                    msg.flags.bits(),
                    msg.body_fetched as u8,
                ])?;
            }
        }
        // Evict excess: delete the lowest UIDs beyond the per-mailbox cap.
        // Also cascade to bodies and attachments so they don't accumulate.
        if messages.len() > self.max_cached_messages_per_mailbox {
            let excess = (messages.len() - self.max_cached_messages_per_mailbox) as i64;
            tx.execute(
                "DELETE FROM message_bodies WHERE (account_name, mailbox_name, uid) IN (
                    SELECT account_name, mailbox_name, uid FROM messages
                    WHERE account_name = ?1 AND mailbox_name = ?2
                    ORDER BY uid ASC LIMIT ?3
                )",
                params![account, mailbox, excess],
            )?;
            tx.execute(
                "DELETE FROM message_attachments WHERE (account_name, mailbox_name, uid) IN (
                    SELECT account_name, mailbox_name, uid FROM messages
                    WHERE account_name = ?1 AND mailbox_name = ?2
                    ORDER BY uid ASC LIMIT ?3
                )",
                params![account, mailbox, excess],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE (account_name, mailbox_name, uid) IN (
                    SELECT account_name, mailbox_name, uid FROM messages
                    WHERE account_name = ?1 AND mailbox_name = ?2
                    ORDER BY uid ASC
                    LIMIT ?3
                )",
                params![account, mailbox, excess],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // -- contacts -----------------------------------------------------------

    pub fn add_contact(&self, name: &str, email: &str) -> SqlResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.conn.execute(
            "INSERT INTO contacts (name, email, seen_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(email) DO UPDATE SET
                 name = excluded.name,
                 seen_at = excluded.seen_at",
            params![name, email, now],
        )?;
        // Evict excess: delete the least-recently-seen contacts.
        self.conn.execute(
            "DELETE FROM contacts WHERE email IN (
                SELECT email FROM contacts
                ORDER BY seen_at ASC
                LIMIT max(0, (SELECT COUNT(*) FROM contacts) - ?1)
            )",
            params![self.max_contacts as i64],
        )?;
        Ok(())
    }

    pub fn search_contacts(&self, prefix: &str) -> SqlResult<Vec<Contact>> {
        let pattern = format!("{prefix}%");
        let mut stmt = self.conn.prepare(
            "SELECT name, email FROM contacts
             WHERE name LIKE ?1 OR email LIKE ?1
             ORDER BY name LIMIT 10",
        )?;
        let rows = stmt.query_map(params![pattern], |row| {
            Ok(Contact {
                name: row.get(0)?,
                email: row.get(1)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    pub fn learn_addresses<'a>(&self, addresses: impl IntoIterator<Item = &'a str>) {
        for addr in addresses {
            let (name, email) = parse_addr(addr);
            if !email.is_empty() {
                self.add_contact(&name, &email).ok();
            }
        }
    }

    // -- bodies -------------------------------------------------------------

    /// Store the parsed body text and optional HTML for a message.
    pub fn cache_body(
        &self,
        account: &str,
        mailbox: &str,
        uid: u32,
        body: &str,
        html_body: Option<&str>,
    ) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO message_bodies
                (account_name, mailbox_name, uid, body_text, html_body)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account, mailbox, uid, body, html_body],
        )?;
        Ok(())
    }

    /// Load cached body text and optional HTML for a message.
    /// Returns `None` if not cached or if the body text is empty.
    pub fn load_body(
        &self,
        account: &str,
        mailbox: &str,
        uid: u32,
    ) -> SqlResult<Option<(String, Option<String>)>> {
        match self.conn.query_row(
            "SELECT body_text, html_body FROM message_bodies
             WHERE account_name = ?1 AND mailbox_name = ?2 AND uid = ?3",
            params![account, mailbox, uid],
            |row| {
                let body: String = row.get(0)?;
                let html: Option<String> = row.get(1).ok();
                Ok((body, html))
            },
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Check whether a body is cached for the given message.
    pub fn has_cached_body(&self, account: &str, mailbox: &str, uid: u32) -> SqlResult<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM message_bodies
             WHERE account_name = ?1 AND mailbox_name = ?2 AND uid = ?3",
            params![account, mailbox, uid],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    // -- attachments --------------------------------------------------------

    /// Store attachment bytes, keyed by (account, mailbox, uid, filename).
    pub fn cache_attachment(
        &self,
        account: &str,
        mailbox: &str,
        uid: u32,
        filename: &str,
        data: &[u8],
    ) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO message_attachments
                (account_name, mailbox_name, uid, filename, data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account, mailbox, uid, filename, data],
        )?;
        Ok(())
    }

    /// Load cached attachment bytes.
    /// Returns `None` if not cached.
    pub fn load_attachment(
        &self,
        account: &str,
        mailbox: &str,
        uid: u32,
        filename: &str,
    ) -> SqlResult<Option<Vec<u8>>> {
        match self.conn.query_row(
            "SELECT data FROM message_attachments
             WHERE account_name = ?1 AND mailbox_name = ?2
               AND uid = ?3 AND filename = ?4",
            params![account, mailbox, uid, filename],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn wipe_mailbox(&self, account: &str, mailbox: &str) -> SqlResult<()> {
        self.conn.execute(
            "DELETE FROM messages WHERE account_name = ?1 AND mailbox_name = ?2",
            params![account, mailbox],
        )?;
        self.conn.execute(
            "DELETE FROM message_bodies WHERE account_name = ?1 AND mailbox_name = ?2",
            params![account, mailbox],
        )?;
        self.conn.execute(
            "DELETE FROM message_attachments WHERE account_name = ?1 AND mailbox_name = ?2",
            params![account, mailbox],
        )?;
        self.conn.execute(
            "DELETE FROM uid_validity WHERE account_name = ?1 AND mailbox_name = ?2",
            params![account, mailbox],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

fn parse_addr(raw: &str) -> (String, String) {
    let raw = raw.trim();
    // "Alice" <alice@example.com>
    if let Some(lt) = raw.rfind('<')
        && let Some(gt) = raw.rfind('>') {
            let email = raw[lt + 1..gt].trim().to_string();
            let name = raw[..lt].trim().trim_matches('"').trim().to_string();
            return (name, email);
        }
    // Plain email
    if raw.contains('@') {
        return (String::new(), raw.to_string());
    }
    (String::new(), String::new())
}

fn run_migrations(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS uid_validity (
            account_name TEXT    NOT NULL,
            mailbox_name TEXT    NOT NULL,
            validity     INTEGER NOT NULL,
            PRIMARY KEY (account_name, mailbox_name)
        );

        CREATE TABLE IF NOT EXISTS messages (
            account_name TEXT    NOT NULL,
            mailbox_name TEXT    NOT NULL,
            uid          INTEGER NOT NULL,
            subject      TEXT    NOT NULL DEFAULT '',
            \"from\"       TEXT    NOT NULL DEFAULT '',
            \"to\"         TEXT    NOT NULL DEFAULT '',
            date         TEXT    NOT NULL DEFAULT '',
            message_id   TEXT    NOT NULL DEFAULT '',
            in_reply_to  TEXT    NOT NULL DEFAULT '',
            refs         TEXT    NOT NULL DEFAULT '',
            flags        INTEGER NOT NULL DEFAULT 0,
            body_fetched INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (account_name, mailbox_name, uid)
        );

        CREATE TABLE IF NOT EXISTS contacts (
            name    TEXT    NOT NULL DEFAULT '',
            email   TEXT    NOT NULL PRIMARY KEY,
            seen_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS message_bodies (
            account_name TEXT NOT NULL,
            mailbox_name TEXT NOT NULL,
            uid          INTEGER NOT NULL,
            body_text    TEXT NOT NULL DEFAULT '',
            html_body    TEXT,
            PRIMARY KEY (account_name, mailbox_name, uid)
        );

        CREATE TABLE IF NOT EXISTS message_attachments (
            account_name TEXT NOT NULL,
            mailbox_name TEXT NOT NULL,
            uid          INTEGER NOT NULL,
            filename     TEXT NOT NULL,
            data         BLOB NOT NULL,
            PRIMARY KEY (account_name, mailbox_name, uid, filename)
        );",
    )?;

    // Migration: add seen_at if missing (for DBs created before v2).
    // SQLite has no IF NOT EXISTS for ALTER TABLE, so we catch the error.
    match conn.execute(
        "ALTER TABLE contacts ADD COLUMN seen_at INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        Ok(_) => {}
        Err(e) => {
            // Error code 1 = "duplicate column name" — already migrated.
            if e.to_string().contains("duplicate column name") {
                // Already has the column, nothing to do.
            } else {
                return Err(e);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache() -> MailboxCache {
        MailboxCache::open_with_limits(":memory:", 10_000, 5_000).expect("open :memory:")
    }

    fn sample_message(uid: u32) -> Message {
        Message::new(
            uid,
            Envelope {
                subject: format!("subject {uid}"),
                from: "a@b.com".into(),
                to: "c@d.com".into(),
                date: "2024-01-01".into(),
                message_id: format!("<{uid}@x>"),
                in_reply_to: String::new(),
                references: format!("<{}@x>", uid.saturating_sub(1)),
            },
            FlagSet::SEEN | FlagSet::FLAGGED,
        )
    }

    #[test]
    fn round_trip_write_then_read() {
        let cache = test_cache();
        let msgs = vec![sample_message(1), sample_message(2), sample_message(3)];

        cache.save_messages("work", "INBOX", &msgs).unwrap();
        let loaded = cache.load_mailbox("work", "INBOX").unwrap();

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].uid, 1);
        assert_eq!(loaded[0].envelope.subject, "subject 1");
        assert!(loaded[0].flags.contains(FlagSet::SEEN));
        assert!(loaded[0].flags.contains(FlagSet::FLAGGED));
        assert_eq!(loaded[2].envelope.references, "<2@x>");
    }

    #[test]
    fn round_trip_preserves_in_reply_to() {
        let cache = test_cache();
        let mut msg = sample_message(1);
        msg.envelope.in_reply_to = "<parent@x>".into();
        msg.envelope.references = "<parent@x> <grandparent@x>".into();

        cache.save_messages("work", "INBOX", &[msg]).unwrap();
        let loaded = cache.load_mailbox("work", "INBOX").unwrap();

        assert_eq!(loaded[0].envelope.in_reply_to, "<parent@x>");
        assert_eq!(loaded[0].envelope.references, "<parent@x> <grandparent@x>");
    }

    #[test]
    fn uid_validity_round_trip() {
        let cache = test_cache();
        assert!(cache.get_uid_validity("work", "INBOX").is_none());

        cache.set_uid_validity("work", "INBOX", 42).unwrap();
        assert_eq!(cache.get_uid_validity("work", "INBOX"), Some(42));

        cache.set_uid_validity("work", "INBOX", 99).unwrap();
        assert_eq!(cache.get_uid_validity("work", "INBOX"), Some(99));

        assert!(cache.get_uid_validity("personal", "INBOX").is_none());
        assert!(cache.get_uid_validity("work", "Archive").is_none());
    }

    #[test]
    fn wipe_clears_messages_and_validity() {
        let cache = test_cache();
        cache.set_uid_validity("work", "INBOX", 1).unwrap();
        cache.save_messages("work", "INBOX", &[sample_message(1)]).unwrap();

        cache.wipe_mailbox("work", "INBOX").unwrap();

        assert!(cache.load_mailbox("work", "INBOX").unwrap().is_empty());
        assert!(cache.get_uid_validity("work", "INBOX").is_none());
    }

    #[test]
    fn cold_start_empty_cache_returns_nothing() {
        let cache = test_cache();
        assert!(cache.load_mailbox("any", "INBOX").unwrap().is_empty());
        assert!(cache.get_uid_validity("any", "INBOX").is_none());
    }

    #[test]
    fn save_replaces_existing_messages() {
        let cache = test_cache();
        cache
            .save_messages("work", "INBOX", &[sample_message(1), sample_message(2)])
            .unwrap();

        let mut updated = sample_message(1);
        updated.envelope.subject = "updated subject".into();
        cache.save_messages("work", "INBOX", &[updated]).unwrap();

        let loaded = cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(loaded.len(), 2, "both messages still present");
        let msg1 = loaded.iter().find(|m| m.uid == 1).unwrap();
        assert_eq!(msg1.envelope.subject, "updated subject");
        let msg2 = loaded.iter().find(|m| m.uid == 2).unwrap();
        assert_eq!(msg2.envelope.subject, "subject 2");
    }

    // -- contacts --------------------------------------------------------

    #[test]
    fn contacts_dedup_on_repeated_add() {
        let cache = test_cache();
        cache.add_contact("Alice", "alice@example.com").unwrap();
        cache.add_contact("Alice A.", "alice@example.com").unwrap(); // should update name
        let results = cache.search_contacts("alice").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Alice A.");
    }

    #[test]
    fn prefix_search_matches_name_and_email() {
        let cache = test_cache();
        cache.add_contact("Bob", "bob@example.com").unwrap();
        cache.add_contact("Carol", "carol@other.com").unwrap();

        let by_name = cache.search_contacts("Bo").unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].email, "bob@example.com");

        let by_email = cache.search_contacts("carol@").unwrap();
        assert_eq!(by_email.len(), 1);
        assert_eq!(by_email[0].name, "Carol");

        let none = cache.search_contacts("zzz").unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn learn_addresses_parses_formats() {
        let cache = test_cache();
        cache.learn_addresses([
            "Alice <alice@a.com>",
            "bob@b.com",
            "\"Carol\" <carol@c.com>",
        ]);
        let results = cache.search_contacts("a").unwrap();
        // Should have all three, but search for "a" only matches alice's email.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Alice");

        // Bob is there too.
        let bob = cache.search_contacts("bob@").unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].name, "");
    }

    #[test]
    fn accounts_are_isolated() {
        let cache = test_cache();
        cache
            .save_messages("work", "INBOX", &[sample_message(1)])
            .unwrap();
        cache
            .save_messages("personal", "INBOX", &[sample_message(10)])
            .unwrap();

        let work_msgs = cache.load_mailbox("work", "INBOX").unwrap();
        let pers_msgs = cache.load_mailbox("personal", "INBOX").unwrap();

        assert_eq!(work_msgs.len(), 1);
        assert_eq!(work_msgs[0].uid, 1);
        assert_eq!(pers_msgs.len(), 1);
        assert_eq!(pers_msgs[0].uid, 10);
    }

    // -- message eviction -------------------------------------------------

    #[test]
    fn message_cache_evicts_oldest_when_limit_exceeded() {
        let cache = MailboxCache::open_with_limits(":memory:", 3, 5000).unwrap();
        // Insert 5 messages — only the 3 highest UIDs should survive.
        let msgs: Vec<Message> = (1..=5).map(sample_message).collect();
        cache.replace_mailbox("work", "INBOX", &msgs).unwrap();

        let loaded = cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(loaded.len(), 3, "only 3 of 5 messages should remain");
        // Lowest UIDs (1, 2) should be evicted.
        let uids: Vec<u32> = loaded.iter().map(|m| m.uid).collect();
        assert_eq!(uids, vec![3, 4, 5]);
    }

    #[test]
    fn message_cache_under_limit_keeps_all() {
        let cache = MailboxCache::open_with_limits(":memory:", 100, 5000).unwrap();
        let msgs: Vec<Message> = (1..=5).map(sample_message).collect();
        cache.replace_mailbox("work", "INBOX", &msgs).unwrap();

        let loaded = cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(loaded.len(), 5);
    }

    #[test]
    fn message_cache_eviction_respects_mailbox_boundaries() {
        let cache = MailboxCache::open_with_limits(":memory:", 2, 5000).unwrap();
        cache
            .replace_mailbox("work", "INBOX", &[sample_message(1), sample_message(2), sample_message(3)])
            .unwrap();
        cache
            .replace_mailbox("work", "Archive", &[sample_message(10), sample_message(20), sample_message(30)])
            .unwrap();

        // Each mailbox independently evicted to 2.
        let inbox = cache.load_mailbox("work", "INBOX").unwrap();
        assert_eq!(inbox.len(), 2);
        let archive = cache.load_mailbox("work", "Archive").unwrap();
        assert_eq!(archive.len(), 2);
    }

    // -- contact eviction --------------------------------------------------

    #[test]
    fn contact_evicts_least_recently_seen() {
        let cache = MailboxCache::open_with_limits(":memory:", 10_000, 3).unwrap();
        cache.add_contact("Alice", "alice@a.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        cache.add_contact("Bob", "bob@b.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        cache.add_contact("Carol", "carol@c.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        cache.add_contact("Dave", "dave@d.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        cache.add_contact("Eve", "eve@e.com").unwrap();

        let all = cache.search_contacts("").unwrap();
        assert_eq!(all.len(), 3, "only 3 of 5 contacts should remain");
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert!(!names.contains(&"Alice"));
        assert!(!names.contains(&"Bob"));
        assert!(names.contains(&"Carol"));
        assert!(names.contains(&"Dave"));
        assert!(names.contains(&"Eve"));
    }

    #[test]
    fn contact_re_seen_updates_eviction_priority() {
        let cache = MailboxCache::open_with_limits(":memory:", 10_000, 3).unwrap();
        cache.add_contact("Alice", "alice@a.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        cache.add_contact("Bob", "bob@b.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        cache.add_contact("Carol", "carol@c.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        // Re-see Alice — bumps her recency timestamp.
        cache.add_contact("Alice Updated", "alice@a.com").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        // Now add a 4th — should evict Bob (oldest seen), not Alice.
        cache.add_contact("Dave", "dave@d.com").unwrap();

        let all = cache.search_contacts("").unwrap();
        assert_eq!(all.len(), 3);
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        // Alice was re-seen, so she should survive.
        assert!(names.contains(&"Alice Updated"), "re-seen contact should survive");
        // Bob was never re-seen — should be evicted.
        assert!(!names.contains(&"Bob"), "least-recently-seen should be evicted");
        assert!(names.contains(&"Carol"));
        assert!(names.contains(&"Dave"));
    }
}
