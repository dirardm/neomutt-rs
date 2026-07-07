//! Threading implementation per Jamie Zawinski's algorithm.
//!
//! Builds a tree of [`ThreadNode`] from a flat [`Mailbox`] using
//! `References` and `In-Reply-To` headers to link parent-child
//! relationships.  The function is pure — no I/O, no async — so it stays
//! unit-testable alongside the rest of core.

use std::collections::HashMap;

use crate::{Envelope, Mailbox, Message};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A node in the thread tree returned by [`thread_mailbox`].
///
/// * `uid` is [`None`] for **dummy** placeholder nodes — these represent a
///   message referenced by `References` or `In-Reply-To` that isn't present
///   in the mailbox (e.g. the start of a thread that arrived before the user
///   subscribed).
/// * `children` are ordered chronologically by the `Date` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadNode {
    /// `None` for dummy placeholder nodes.
    pub uid: Option<u32>,
    /// Child nodes, ordered by date.
    pub children: Vec<ThreadNode>,
}

/// Build a threaded view of `mailbox`.
///
/// Returns a flat list of root [`ThreadNode`]s, each potentially containing
/// children.  Roots are sorted by the date of their oldest descendant.
pub fn thread_mailbox(mailbox: &Mailbox) -> Vec<ThreadNode> {
    thread_messages(&mailbox.messages)
}

/// Build a threaded view from a slice of [`Message`]s.
pub fn thread_messages(messages: &[Message]) -> Vec<ThreadNode> {
    if messages.is_empty() {
        return vec![];
    }

    let mut containers: Vec<Container> = Vec::with_capacity(messages.len());
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();

    // Phase 1: create containers and populate the ID → index map.
    // Normalise message-IDs by stripping angle brackets so they match the
    // IDs produced by `parse_references`.
    for msg in messages {
        let idx = containers.len();
        let norm_id = strip_brackets(&msg.envelope.message_id);
        if !norm_id.is_empty() {
            id_to_idx.insert(norm_id, idx);
        }
        containers.push(Container {
            uid: Some(msg.uid),
            date: msg.envelope.date.clone(),
            parent: None,
            children: Vec::new(),
        });
    }

    // Phase 2: link children to parents.
    for msg in messages {
        let norm_id = strip_brackets(&msg.envelope.message_id);
        let child_idx = id_to_idx.get(&norm_id).copied();
        let Some(child_idx) = child_idx else {
            // Message has no (or empty) Message-ID — treat as root.
            continue;
        };

        let refs = parse_references(&msg.envelope);
        let parent_idx = find_parent(&refs, &mut id_to_idx, &mut containers);

        if let Some(p_idx) = parent_idx {
            containers[p_idx].children.push(child_idx);
            containers[child_idx].parent = Some(p_idx);
        }
    }

    // Phase 3: collect roots and sort recursively.
    let mut roots: Vec<usize> = containers
        .iter()
        .enumerate()
        .filter_map(|(i, c)| if c.parent.is_none() { Some(i) } else { None })
        .collect();

    // Sort roots by the date of their oldest descendant.
    sort_containers_by_date(&mut roots, &containers);

    // Build the tree.
    roots
        .into_iter()
        .map(|idx| build_node(idx, &containers))
        .collect()
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

struct Container {
    uid: Option<u32>,
    date: String,
    parent: Option<usize>,
    children: Vec<usize>,
}

/// Strip surrounding angle brackets from a message-ID.
fn strip_brackets(s: &str) -> String {
    s.trim().trim_matches('<').trim_matches('>').to_owned()
}

/// Split the `References` header into individual message-IDs.  Falls back
/// to `In-Reply-To` if `References` is empty.
fn parse_references(env: &Envelope) -> Vec<String> {
    let raw = if env.references.is_empty() {
        &env.in_reply_to
    } else {
        &env.references
    };

    // References is a space-separated list.  Some MUAs separate with commas
    // too, so be lenient.
    raw.split([' ', ','])
        .map(strip_brackets)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Walk refs right-to-left (newest first) looking for a known parent.
///
/// Returns the container index of the first matching reference, or a newly
/// created dummy container if no match was found but refs were present.
fn find_parent(
    refs: &[String],
    id_to_idx: &mut HashMap<String, usize>,
    containers: &mut Vec<Container>,
) -> Option<usize> {
    if refs.is_empty() {
        return None;
    }

    // Walk right-to-left: the rightmost ref is the most immediate parent.
    for ref_id in refs.iter().rev() {
        if let Some(&idx) = id_to_idx.get(ref_id) {
            return Some(idx);
        }
    }

    // No matching parent found.  Per JWZ, create a dummy for the leftmost
    // (oldest) reference that hasn't been seen yet.  We check whether a
    // dummy for this ID already exists before creating a new one.
    let orphan_id = &refs[0]; // leftmost = oldest ancestor

    // Always create a fresh dummy container.  Duplicate dummies for the
    // same orphan ID are harmless — empty dummies without real children
    // won't appear in the final tree since only containers with real
    // descendants survive the root-collection phase.
    let dummy_idx = containers.len();
    containers.push(Container {
        uid: None,
        date: String::new(),
        parent: None,
        children: Vec::new(),
    });
    // Also register it so that later messages referencing the same orphan
    // find the dummy and attach under it.
    id_to_idx.insert(orphan_id.clone(), dummy_idx);

    Some(dummy_idx)
}

/// Recursively sort `indices` and their descendants by `Date`.
fn sort_containers_by_date(indices: &mut [usize], containers: &[Container]) {
    indices.sort_by(|&a, &b| {
        let date_a = oldest_date(a, containers);
        let date_b = oldest_date(b, containers);
        date_a.cmp(date_b)
    });
    // Children are sorted independently by build_node() using the same
    // oldest_date comparison, so no recursive call is needed here.
}

/// Return the date of the oldest descendant (or self) for sorting purposes.
fn oldest_date(root: usize, containers: &[Container]) -> &str {
    let mut best = containers[root].date.as_str();
    for &child in &containers[root].children {
        let child_best = oldest_date(child, containers);
        if child_best < best || best.is_empty() {
            best = child_best;
        }
    }
    best
}

/// Recursively build a [`ThreadNode`] from the container at `idx`.
fn build_node(idx: usize, containers: &[Container]) -> ThreadNode {
    let c = &containers[idx];

    // Collect and sort children.
    let mut child_indices = c.children.clone();
    // Sort by oldest-date within each child subtree.
    child_indices.sort_by(|&a, &b| {
        let date_a = oldest_date(a, containers);
        let date_b = oldest_date(b, containers);
        date_a.cmp(date_b)
    });

    let children: Vec<ThreadNode> = child_indices
        .into_iter()
        .map(|i| build_node(i, containers))
        .collect();

    ThreadNode {
        uid: c.uid,
        children,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FlagSet;

    fn msg(uid: u32, subject: &str, date: &str, msg_id: &str, refs: &str) -> Message {
        Message::new(
            uid,
            Envelope {
                subject: subject.into(),
                from: String::new(),
                to: String::new(),
                date: date.into(),
                message_id: msg_id.into(),
                in_reply_to: String::new(),
                references: refs.into(),
            },
            FlagSet::default(),
        )
    }

    fn msg_with_irt(
        uid: u32,
        subject: &str,
        date: &str,
        msg_id: &str,
        irt: &str,
    ) -> Message {
        Message::new(
            uid,
            Envelope {
                subject: subject.into(),
                from: String::new(),
                to: String::new(),
                date: date.into(),
                message_id: msg_id.into(),
                in_reply_to: irt.into(),
                references: String::new(),
            },
            FlagSet::default(),
        )
    }

    #[test]
    fn simple_three_message_thread() {
        // msg1 is the root, msg2 replies to msg1, msg3 replies to msg2.
        let messages = vec![
            msg(1, "Original", "2024-01-01", "<1@x>", ""),
            msg(2, "Re: Original", "2024-01-02", "<2@x>", "<1@x>"),
            msg(3, "Re: Re: Original", "2024-01-03", "<3@x>", "<1@x> <2@x>"),
        ];

        let roots = thread_messages(&messages);
        assert_eq!(roots.len(), 1, "should have one root thread");

        let root = &roots[0];
        assert_eq!(root.uid, Some(1));
        assert_eq!(root.children.len(), 1);

        let child = &root.children[0];
        assert_eq!(child.uid, Some(2));
        assert_eq!(child.children.len(), 1);

        let grandchild = &child.children[0];
        assert_eq!(grandchild.uid, Some(3));
        assert!(grandchild.children.is_empty());
    }

    #[test]
    fn orphan_references_creates_dummy() {
        // msg references an ID not in the mailbox — should create a dummy root.
        let messages = vec![msg(1, "Reply", "2024-01-02", "<2@x>", "<nonexistent@x>")];

        let roots = thread_messages(&messages);
        assert_eq!(roots.len(), 1);

        // The root should be a dummy node (None uid), with our message as child.
        let root = &roots[0];
        assert_eq!(root.uid, None);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].uid, Some(1));
    }

    #[test]
    fn in_reply_to_used_as_fallback() {
        // When References is empty, In-Reply-To provides the parent link.
        let messages = vec![
            msg(1, "Original", "2024-01-01", "<1@x>", ""),
            msg_with_irt(2, "Reply", "2024-01-02", "<2@x>", "<1@x>"),
        ];

        let roots = thread_messages(&messages);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].uid, Some(1));
        assert_eq!(roots[0].children.len(), 1);
        assert_eq!(roots[0].children[0].uid, Some(2));
    }

    #[test]
    fn multiple_independent_threads() {
        let messages = vec![
            // Thread A: 1 -> 2
            msg(1, "A root", "2024-01-01", "<a1>", ""),
            msg(2, "Re: A", "2024-01-02", "<a2>", "<a1>"),
            // Thread B: 3 -> 4
            msg(3, "B root", "2024-01-03", "<b1>", ""),
            msg(4, "Re: B", "2024-01-04", "<b2>", "<b1>"),
            // Thread C: lone message
            msg(5, "Lone", "2024-01-05", "<c1>", ""),
        ];

        let roots = thread_messages(&messages);
        assert_eq!(roots.len(), 3, "three independent threads");

        // They should be sorted by date: A (Jan 1), B (Jan 3), C (Jan 5).
        assert_eq!(roots[0].uid, Some(1)); // A root
        assert_eq!(roots[1].uid, Some(3)); // B root
        assert_eq!(roots[2].uid, Some(5)); // C root

        // A has child 2
        assert_eq!(roots[0].children.len(), 1);
        assert_eq!(roots[0].children[0].uid, Some(2));

        // B has child 4
        assert_eq!(roots[1].children.len(), 1);
        assert_eq!(roots[1].children[0].uid, Some(4));

        // C is alone
        assert!(roots[2].children.is_empty());
    }

    #[test]
    fn empty_mailbox_yields_no_roots() {
        let roots = thread_messages(&[]);
        assert!(roots.is_empty());
    }

    #[test]
    fn message_without_message_id_becomes_root() {
        let messages = vec![msg(1, "No ID", "2024-01-01", "", "")];
        let roots = thread_messages(&messages);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].uid, Some(1));
        assert!(roots[0].children.is_empty());
    }
}
