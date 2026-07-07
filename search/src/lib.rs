//! Full-text search over mailbox messages via `tantivy`.
//!
//! Each message is indexed by `(account, mailbox, uid)` with subject, from,
//! and optionally body fields.  Updates are incremental — old entries for the
//! same uid are deleted before new ones are inserted.

use std::collections::HashMap;
use std::path::Path;

use neomutt_core::Message;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::*;
use tantivy::{doc, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score};

// ---------------------------------------------------------------------------
// SearchIndex
// ---------------------------------------------------------------------------

pub struct SearchIndex {
    index: Index,
    schema: SearchSchema,
    writer: IndexWriter,
    reader: IndexReader,
    /// Maximum number of indexed messages across all accounts.
    max_indexed_messages: usize,
    /// Ordered list of `(uid_key)` per account, in insertion order.
    /// Used to evict the oldest entries when max_indexed_messages is exceeded.
    insertion_order: HashMap<String, Vec<String>>,
}

struct SearchSchema {
    uid_field: Field,
    account_field: Field,
    mailbox_field: Field,
    subject_field: Field,
    from_field: Field,
    body_field: Field,
    #[allow(dead_code)]
    schema: Schema,
}

/// A light-weight handle that can be cloned and sent across threads for
/// search queries.  It holds only the readable parts of the index.
#[derive(Clone)]
pub struct SearchHandle {
    index: Index,
    reader: IndexReader,
    subject_field: Field,
    from_field: Field,
    body_field: Field,
    uid_field: Field,
    account_field: Field,
    mailbox_field: Field,
}

impl SearchHandle {
    /// Run a search query, optionally scoped to an account/mailbox.
    /// Returns `(uid, score)` pairs ordered by relevance.
    pub fn search(
        &self,
        query_str: &str,
        account: Option<&str>,
        mailbox: Option<&str>,
    ) -> tantivy::Result<Vec<(u32, f32)>> {
        do_search(
            &self.index,
            &self.reader,
            self.subject_field,
            self.from_field,
            self.body_field,
            self.uid_field,
            self.account_field,
            self.mailbox_field,
            query_str,
            account,
            mailbox,
        )
    }
}

impl SearchIndex {
    /// Return a `Send + Sync + Clone` handle suitable for use in
    /// `spawn_blocking`.
    pub fn reader_handle(&self) -> SearchHandle {
        SearchHandle {
            index: self.index.clone(),
            reader: self.reader.clone(),
            subject_field: self.schema.subject_field,
            from_field: self.schema.from_field,
            body_field: self.schema.body_field,
            uid_field: self.schema.uid_field,
            account_field: self.schema.account_field,
            mailbox_field: self.schema.mailbox_field,
        }
    }

    /// Open or create an index at `path`.
    pub fn open(
        path: &Path,
        writer_buffer_bytes: usize,
        max_indexed_messages: usize,
    ) -> tantivy::Result<Self> {
        let (schema_fields, schema) = build_schema();

        let index = if path.join("meta.json").exists() {
            Index::open_in_dir(path)?
        } else {
            std::fs::create_dir_all(path)?;
            Index::create_in_dir(path, schema.clone())?
        };

        let writer = index.writer(writer_buffer_bytes)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        Ok(Self {
            index,
            schema: schema_fields,
            writer,
            reader,
            max_indexed_messages,
            insertion_order: HashMap::new(),
        })
    }

    /// Index (or re-index) messages for an account+mailbox.
    ///
    /// Deletes old entries for the same UIDs before inserting.
    /// Evicts the oldest entries (by insertion order, across all accounts)
    /// when `max_indexed_messages` is exceeded.
    pub fn index_messages(
        &mut self,
        account: &str,
        mailbox: &str,
        messages: &[Message],
    ) -> tantivy::Result<()> {
        let account_key = account.to_owned();

        for msg in messages {
            let uid_term =
                make_uid_term(&self.schema, account, mailbox, msg.uid);
            self.writer.delete_term(uid_term);

            let uid_key = format_uid_key(account, mailbox, msg.uid);
            // Update insertion order: remove old if re-indexing, append new.
            let list = self.insertion_order.entry(account_key.clone()).or_default();
            list.retain(|k| k != &uid_key);
            list.push(uid_key);

            let mut doc = doc!(
                self.schema.uid_field     => list.last().unwrap().clone(),
                self.schema.account_field => account,
                self.schema.mailbox_field => mailbox,
                self.schema.subject_field => msg.envelope.subject.as_str(),
                self.schema.from_field    => msg.envelope.from.as_str(),
            );

            if msg.body_fetched {
                doc.add_text(self.schema.body_field, "");
            }

            self.writer.add_document(doc)?;
        }

        // Evict oldest entries across all accounts when limit exceeded.
        let total: usize = self.insertion_order.values().map(|v| v.len()).sum();
        if total > self.max_indexed_messages {
            let excess = total - self.max_indexed_messages;
            // Fair eviction: remove proportionally from each account.
            // We evict from the front (oldest) of each account's list.
            let mut evicted = 0usize;
            while evicted < excess {
                let mut evicted_this_round = false;
                for list in self.insertion_order.values_mut() {
                    if evicted >= excess {
                        break;
                    }
                    if let Some(oldest) = list.first().cloned() {
                        // Delete from Tantivy index.
                        let term = tantivy::Term::from_field_text(
                            self.schema.uid_field,
                            &oldest,
                        );
                        self.writer.delete_term(term);
                        list.remove(0);
                        evicted += 1;
                        evicted_this_round = true;
                    }
                }
                if !evicted_this_round {
                    break; // nothing left to evict
                }
            }
        }

        self.writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// Run a query via the built-in reader.
    pub fn search(
        &self,
        query_str: &str,
        account: Option<&str>,
        mailbox: Option<&str>,
    ) -> tantivy::Result<Vec<(u32, f32)>> {
        do_search(
            &self.index,
            &self.reader,
            self.schema.subject_field,
            self.schema.from_field,
            self.schema.body_field,
            self.schema.uid_field,
            self.schema.account_field,
            self.schema.mailbox_field,
            query_str,
            account,
            mailbox,
        )
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn build_schema() -> (SearchSchema, Schema) {
    let mut sb = Schema::builder();
    let uid_field = sb.add_text_field("uid", STRING | STORED);
    let account_field = sb.add_text_field("account", STRING | STORED);
    let mailbox_field = sb.add_text_field("mailbox", STRING | STORED);
    let subject_field = sb.add_text_field("subject", TEXT | STORED);
    let from_field = sb.add_text_field("from", TEXT | STORED);
    let body_field = sb.add_text_field("body", TEXT);
    let schema = sb.build();
    (
        SearchSchema {
            uid_field,
            account_field,
            mailbox_field,
            subject_field,
            from_field,
            body_field,
            schema: schema.clone(),
        },
        schema,
    )
}

fn format_uid_key(account: &str, mailbox: &str, uid: u32) -> String {
    format!("{account}:{mailbox}:{uid}")
}

#[allow(clippy::too_many_arguments)]
fn do_search(
    index: &Index,
    reader: &IndexReader,
    subject_field: Field,
    from_field: Field,
    body_field: Field,
    uid_field: Field,
    account_field: Field,
    mailbox_field: Field,
    query_str: &str,
    account: Option<&str>,
    mailbox: Option<&str>,
) -> tantivy::Result<Vec<(u32, f32)>> {
    let searcher = reader.searcher();
    let fields = vec![subject_field, from_field, body_field];
    let query_parser = QueryParser::for_index(index, fields);
    let text_query = query_parser.parse_query(query_str)?;

    let query: Box<dyn tantivy::query::Query> = if let Some(acct) = account {
        let acct_term = tantivy::Term::from_field_text(account_field, acct);
        let mut subqueries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![
            (Occur::Must, Box::new(text_query)),
            (
                Occur::Must,
                Box::new(TermQuery::new(acct_term, IndexRecordOption::Basic)),
            ),
        ];
        if let Some(mb) = mailbox {
            let mb_term = tantivy::Term::from_field_text(mailbox_field, mb);
            subqueries.push((
                Occur::Must,
                Box::new(TermQuery::new(mb_term, IndexRecordOption::Basic)),
            ));
        }
        Box::new(BooleanQuery::new(subqueries))
    } else {
        Box::new(text_query)
    };

    let collector = TopDocs::with_limit(50).order_by_score();
    let top_docs: Vec<(Score, DocAddress)> = searcher.search(&query, &collector)?;

    let mut results = Vec::new();
    for (_score, addr) in top_docs {
        let doc = searcher.doc::<TantivyDocument>(addr)?;
        if let Some(uid_str) = doc.get_first(uid_field).and_then(|v| v.as_str())
            && let Some(uid) =
                uid_str.rsplit(':').next().and_then(|s| s.parse::<u32>().ok())
            {
                results.push((uid, _score));
            }
    }
    Ok(results)
}

fn make_uid_term(
    schema: &SearchSchema,
    account: &str,
    mailbox: &str,
    uid: u32,
) -> tantivy::Term {
    tantivy::Term::from_field_text(
        schema.uid_field,
        &format_uid_key(account, mailbox, uid),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use neomutt_core::{Envelope, FlagSet};

    fn msg(uid: u32, subject: &str, from: &str, body_fetched: bool) -> Message {
        Message {
            attachments: Vec::new(),
            body: String::new(),
            html_body: None,
            uid,
            envelope: Envelope {
                subject: subject.into(),
                from: from.into(),
                to: String::new(),
                date: "2024-01-01".into(),
                message_id: format!("<{uid}@x>"),
                in_reply_to: String::new(),
                references: String::new(),
            },
            flags: FlagSet::default(),
            body_fetched,
        }
    }

    fn build_index() -> (SearchIndex, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let idx = SearchIndex::open(dir.path(), 50_000_000, 50_000).unwrap();
        (idx, dir)
    }

    #[test]
    fn subject_search_finds_match() {
        let (mut idx, _dir) = build_index();
        idx.index_messages(
            "work",
            "INBOX",
            &[
                msg(1, "Project update", "alice@x.com", false),
                msg(2, "Lunch plans", "bob@x.com", false),
                msg(3, "Project review", "carol@x.com", false),
            ],
        )
        .unwrap();

        let results = idx.search("project", Some("work"), Some("INBOX")).unwrap();
        assert_eq!(results.len(), 2);
        let uids: Vec<u32> = results.iter().map(|(u, _)| *u).collect();
        assert!(uids.contains(&1));
        assert!(uids.contains(&3));
    }

    #[test]
    fn from_search_finds_sender() {
        let (mut idx, _dir) = build_index();
        idx.index_messages(
            "work",
            "INBOX",
            &[
                msg(1, "Subj", "alice@x.com", false),
                msg(2, "Subj", "bob@x.com", false),
            ],
        )
        .unwrap();

        let results = idx
            .search("from:alice", Some("work"), Some("INBOX"))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn account_scope_isolates_results() {
        let (mut idx, _dir) = build_index();
        idx.index_messages(
            "work",
            "INBOX",
            &[msg(1, "Secret project", "a@x", false)],
        )
        .unwrap();
        idx.index_messages(
            "personal",
            "INBOX",
            &[msg(10, "Personal project", "b@x", false)],
        )
        .unwrap();

        let work = idx.search("project", Some("work"), Some("INBOX")).unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].0, 1);

        let pers = idx
            .search("project", Some("personal"), Some("INBOX"))
            .unwrap();
        assert_eq!(pers.len(), 1);
        assert_eq!(pers[0].0, 10);
    }

    #[test]
    fn update_replaces_old_entry() {
        let (mut idx, _dir) = build_index();
        idx.index_messages(
            "work",
            "INBOX",
            &[msg(1, "old subject", "a@x", false)],
        )
        .unwrap();
        idx.index_messages(
            "work",
            "INBOX",
            &[msg(1, "new subject", "a@x", false)],
        )
        .unwrap();

        assert!(idx
            .search("old", Some("work"), Some("INBOX"))
            .unwrap()
            .is_empty());
        let new = idx
            .search("new", Some("work"), Some("INBOX"))
            .unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].0, 1);
    }

    #[test]
    fn no_account_scope_searches_all() {
        let (mut idx, _dir) = build_index();
        idx.index_messages(
            "work",
            "INBOX",
            &[msg(1, "alpha", "a@x", false)],
        )
        .unwrap();
        idx.index_messages(
            "personal",
            "INBOX",
            &[msg(10, "beta", "b@x", false)],
        )
        .unwrap();

        assert_eq!(
            idx.search("alpha", None, None).unwrap().len(),
            1
        );
        assert_eq!(
            idx.search("beta", None, None).unwrap()[0].0,
            10
        );
    }

    // -- eviction -----------------------------------------------------------

    #[test]
    fn search_evicts_oldest_when_limit_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = SearchIndex::open(dir.path(), 50_000_000, 3).unwrap();
        // Index 5 messages — only 3 should survive (oldest 2 evicted).
        idx.index_messages(
            "work", "INBOX",
            &[
                msg(1, "msg one", "a@x", false),
                msg(2, "msg two", "b@x", false),
                msg(3, "msg three", "c@x", false),
                msg(4, "msg four", "d@x", false),
                msg(5, "msg five", "e@x", false),
            ],
        ).unwrap();

        // Oldest (1, 2) should be evicted.
        assert!(idx.search("one", Some("work"), Some("INBOX")).unwrap().is_empty());
        assert!(idx.search("two", Some("work"), Some("INBOX")).unwrap().is_empty());
        // Newest (3, 4, 5) should survive.
        assert_eq!(idx.search("three", Some("work"), Some("INBOX")).unwrap().len(), 1);
        assert_eq!(idx.search("five", Some("work"), Some("INBOX")).unwrap().len(), 1);
    }

    #[test]
    fn search_eviction_respects_account_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = SearchIndex::open(dir.path(), 50_000_000, 4).unwrap();
        // Index 3 messages per account (6 total, limit 4).
        idx.index_messages("work", "INBOX", &[
            msg(1, "work one", "a@x", false),
            msg(2, "work two", "a@x", false),
            msg(3, "work three", "a@x", false),
        ]).unwrap();
        idx.index_messages("personal", "INBOX", &[
            msg(10, "pers one", "b@x", false),
            msg(20, "pers two", "b@x", false),
            msg(30, "pers three", "b@x", false),
        ]).unwrap();

        // Total is 6, limit 4. Fair eviction removes ~1 from each account.
        let work = idx.search("work", Some("work"), Some("INBOX")).unwrap();
        let pers = idx.search("pers", Some("personal"), Some("INBOX")).unwrap();
        // Both accounts should still have results (neither fully evicted).
        assert!(work.len() >= 1, "work account should retain some messages");
        assert!(pers.len() >= 1, "personal account should retain some messages");
        // Total should be at most 4.
        assert!(work.len() + pers.len() <= 4);
    }

    #[test]
    fn search_under_limit_keeps_all() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = SearchIndex::open(dir.path(), 50_000_000, 100).unwrap();
        idx.index_messages("work", "INBOX", &[
            msg(1, "a", "x@x", false),
            msg(2, "b", "x@x", false),
            msg(3, "c", "x@x", false),
        ]).unwrap();
        assert_eq!(idx.search("a", Some("work"), Some("INBOX")).unwrap().len(), 1);
        assert_eq!(idx.search("b", Some("work"), Some("INBOX")).unwrap().len(), 1);
        assert_eq!(idx.search("c", Some("work"), Some("INBOX")).unwrap().len(), 1);
    }
}
