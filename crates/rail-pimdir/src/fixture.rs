//! Pimdir stores for tests, in the state a neverest sync of an IMAP account leaves them.
//!
//! What a run writes that a reader or a producer can see, mirrored here:
//!
//! - **The source's capabilities** (pimdir STORAGE §15.6), declared on every run: what an
//!   IMAP source with an SMTP table and neverest's default permissions supports, the rows
//!   neverest's `offline::capability::{supports, imap, declaration}` derive. Producers branch
//!   on whether an account declared any, so an undeclared fixture would test a path a real
//!   store never takes. `tests/neverest.rs` compares these rows with a real run's.
//! - **The syncing row** of each collection, as `offline::storage::register_syncing` records
//!   it, with the same canonical statements.
//! - **Each message as a reconciled, hydrated placement**: the body stored, the placement
//!   `Full` with the summary and sort key derived from the body, and a `base` naming that body
//!   and flags, as after a first reconcile. (A real IMAP run reaches that state in two steps,
//!   a header-only `Meta` placement and then its hydration; a fixture writes the end state.)
//!
//! Not mirrored: checkpoints (a fixture records none, as for a collection listed but not yet
//! delta-synced), rounds and coverage, which only the sync engine reads.
//!
//! Collection ids are `<source>/<mailbox>`, as neverest's `hub_id` makes them. In a store
//! several accounts share, every account's source needs its own name, or two accounts' inboxes
//! become one collection (`tests/shared_store.rs` shows it).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use io_pimdir::capability::{
    self, COLLECTION_CREATE, COLLECTION_DELETE, COLLECTION_RENAME, MAIL_FLAGS_KEYWORDS,
    PimdirCapability, PimdirSupport,
};
use io_pimdir::change::PimdirWriteOp;
use io_pimdir::client::reader::PimdirItem;
use io_pimdir::client::search::{PimdirIndex, PimdirIndexRefreshOptions};
use io_pimdir::client::{PimdirError, PimdirSourceStore, PimdirStore};
use io_pimdir::collection::PimdirCollectionId;
use io_pimdir::object::PimdirObject;
use io_pimdir::placement::{
    PimdirBase, PimdirFlags, PimdirHandle, PimdirLevel, PimdirLinkId, PimdirPlacement,
    PimdirSortKey, PimdirStatus,
};
use io_pimdir::sql;
use io_pimdir::summary::mail::PimdirMailSummary;
use io_pimdir::summary::{self, PimdirSummary};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, named_params};

/// The media type of a mail collection (pimdir STORAGE §13).
pub const MAIL: &str = "message/rfc822";

// neverest's capability details, verbatim (offline/capability.rs), so the rows compare equal.
const HELD_ONLY: &str = "performed on the collections this source syncs only";
const FIXED: &str = "the provider's own collection, stating a role, stays as it is";
const DELETE_DISABLED: &str = "collection.delete is disabled";
const KEYWORDS_PARTIAL: &str = "as far as the server's PERMANENTFLAGS allow";

/// One message to land: its source-side handle (an IMAP UID, a Gmail id), its raw RFC 5322
/// bytes and its flags (`\Seen`, `\Flagged`, …).
#[derive(Clone, Debug)]
pub struct Message {
    pub handle: String,
    pub body: Vec<u8>,
    pub flags: Vec<String>,
}

impl Message {
    pub fn new(handle: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            handle: handle.into(),
            body: body.into(),
            flags: Vec::new(),
        }
    }

    /// Adds flags, IMAP spelling.
    pub fn flags(mut self, flags: &[&str]) -> Self {
        self.flags
            .extend(flags.iter().map(|flag| (*flag).to_owned()));
        self
    }
}

/// Where one landed message went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    /// Its public id, shared by every placement of it (pimdir STORAGE §9.1).
    pub seq: i64,
    /// Its identity as the store keys it, derived from the Message-ID (Annex A).
    pub link_id: String,
}

/// A store being built. Holds no handle between calls, so a test can open readers on it at any
/// point, as Rail does while neverest is not running.
#[derive(Clone, Debug)]
pub struct Fixture {
    dir: PathBuf,
}

/// One mailbox of one account, as a source syncs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mailbox {
    pub account: String,
    pub source: String,
    /// The collection id: `<source>/<name>`.
    pub id: String,
}

impl Fixture {
    /// Creates an empty store under `dir`.
    pub fn create(dir: impl AsRef<Path>) -> Result<Self, PimdirError> {
        let dir = dir.as_ref().to_path_buf();
        drop(PimdirStore::open(&dir)?);
        Ok(Self { dir })
    }

    /// The store's directory, what Rail opens.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn source(&self, account: &str, source: &str) -> Result<PimdirSourceStore, PimdirError> {
        Ok(PimdirStore::open(&self.dir)?
            .for_account(account)
            .for_source(source))
    }

    /// Declares the mailbox `name` of `account`, synced by `source`, with its role (`inbox`,
    /// `sent`, `drafts`, `trash`, `junk`, `archive`, `all`, `flagged`, `important`) when the
    /// server states one.
    pub fn mailbox(
        &self,
        account: &str,
        source: &str,
        name: &str,
        role: Option<&str>,
    ) -> Result<Mailbox, PimdirError> {
        let id = format!("{source}/{name}");
        let store = self.source(account, source)?;
        store.ensure_collection(&id, MAIL)?;
        store.set_collection_name(&id, name)?;
        if role.is_some() {
            store.set_collection_role(&id, role)?;
        }
        drop(store);
        register_syncing(&self.dir, source, &id)?;
        // neverest re-declares on every run, from the collections the source then holds.
        let mut store = self.source(account, source)?;
        let declared = imap_declaration(&store, account, source)?;
        store.declare(source, &declared)?;
        Ok(Mailbox {
            account: account.to_owned(),
            source: source.to_owned(),
            id,
        })
    }

    /// Lands `messages` in `mailbox` as one sync batch. Answers where each landed, in order.
    /// A message landed in two mailboxes (a Gmail label) keeps one `seq`, one link id and one
    /// body.
    pub fn land(
        &self,
        mailbox: &Mailbox,
        messages: &[Message],
    ) -> Result<Vec<Landed>, PimdirError> {
        let mut store = self.source(&mailbox.account, &mailbox.source)?;
        let mut ops = Vec::with_capacity(messages.len() * 2);
        let mut links = Vec::with_capacity(messages.len());
        for message in messages {
            let derived = summary::derive(MAIL, &message.body)
                .expect("message/rfc822 always derives a summary");
            let hash = store.hash(&message.body);
            links.push(derived.link_id.0.clone());
            ops.push(PimdirWriteOp::StoreObject {
                object: PimdirObject {
                    hash: hash.clone(),
                    size: message.body.len(),
                },
                body: Some(message.body.clone()),
            });
            let flags = PimdirFlags::Known(message.flags.iter().cloned().collect::<BTreeSet<_>>());
            ops.push(PimdirWriteOp::UpsertPlacement(PimdirPlacement {
                collection: PimdirCollectionId(mailbox.id.clone()),
                handle: PimdirHandle(message.handle.clone()),
                link_id: Some(PimdirLinkId(derived.link_id.0)),
                object: Some(hash.clone()),
                level: PimdirLevel::Full,
                summary: derived.summary,
                sort_key: PimdirSortKey(derived.sort_key.0),
                flags: flags.clone(),
                status: PimdirStatus::Clean,
                conflict_revision: None,
                conflict_object: None,
                // As after a first reconcile: the source last synced these flags and this body.
                base: Some(PimdirBase {
                    flags,
                    revision: None,
                    object: Some(hash),
                }),
                origin: None,
            }));
        }
        store.write(ops)?;
        links
            .iter()
            .map(|link| {
                let placements = store.link_placements(link)?;
                let first = placements
                    .first()
                    .unwrap_or_else(|| panic!("{link} was written but landed nowhere"));
                Ok(Landed {
                    seq: first.seq,
                    link_id: link.clone(),
                })
            })
            .collect()
    }

    /// Brings the search index (threads, the query language) up to date with everything
    /// landed, as neverest does after a run. Floating times read in UTC, so fixtures don't
    /// depend on the machine's zone.
    pub fn index(&self) -> Result<(), PimdirError> {
        let mut index = PimdirIndex::open(&self.dir)?;
        index.refresh(&PimdirIndexRefreshOptions {
            deadline: None,
            floating_zone: "UTC".into(),
            ..Default::default()
        })?;
        Ok(())
    }
}

/// The capability rows neverest declares for an IMAP source with an SMTP table, two-way and
/// unpaired, under its default permissions (collection.create on, collection.delete off,
/// flag.update and item.create/delete/update on): `supports` + `imap` + `declaration` in its
/// offline/capability.rs. The collection-scoped intents are held-only: a `none` source-wide row
/// and a row per collection the source holds, a role-bearing collection kept from renames.
fn imap_declaration(
    store: &PimdirSourceStore,
    account: &str,
    source: &str,
) -> Result<Vec<PimdirCapability>, PimdirError> {
    let prefix = format!("{source}/");
    let mut held = Vec::new();
    for collection in store.list_collections_by_account(Some(account))? {
        let syncing = store.collection_sources(&collection.id)?;
        if syncing.iter().any(|name| name == source) || collection.id.starts_with(&prefix) {
            held.push((collection.id, collection.role.is_some()));
        }
    }
    let row =
        |collection: Option<&str>, name: &str, support, detail: Option<&str>| PimdirCapability {
            collection: collection.map(String::from),
            name: name.to_owned(),
            support,
            detail: detail.map(String::from),
        };
    let mut rows = Vec::new();
    for &name in capability::MAIL {
        match name {
            COLLECTION_DELETE => {
                rows.push(row(None, name, PimdirSupport::None, Some(DELETE_DISABLED)))
            }
            COLLECTION_CREATE | COLLECTION_RENAME => {
                rows.push(row(None, name, PimdirSupport::None, Some(HELD_ONLY)));
                for (collection, fixed) in &held {
                    rows.push(match name == COLLECTION_RENAME && *fixed {
                        true => row(Some(collection), name, PimdirSupport::None, Some(FIXED)),
                        false => row(Some(collection), name, PimdirSupport::Full, None),
                    });
                }
            }
            MAIL_FLAGS_KEYWORDS => rows.push(row(
                None,
                name,
                PimdirSupport::Partial,
                Some(KEYWORDS_PARTIAL),
            )),
            _ => rows.push(row(None, name, PimdirSupport::Full, None)),
        }
    }
    Ok(rows)
}

/// Records `source` as syncing `collection` with no checkpoint yet, unless a row is there:
/// neverest's `register_syncing`, with the canonical statements it runs. io-pimdir has no
/// owner write for it, so neverest holds a connection of its own, and so does this.
fn register_syncing(dir: &Path, source: &str, collection: &str) -> Result<(), PimdirError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(dir.join("pimdir.db"), flags)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 30000;")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let key = named_params! { ":collection": collection, ":source": source };
    let held = tx
        .query_row(sql::LOAD_CHECKPOINT, key, |_| Ok(()))
        .optional()?
        .is_some();
    if !held {
        tx.execute(
            sql::UPSERT_CHECKPOINT,
            named_params! {
                ":collection": collection,
                ":source": source,
                ":checkpoint": None::<Vec<u8>>,
            },
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// The mail summary of `item`. Panics on an item that is not mail or was never summarized,
/// which a fixture's mail never is.
pub fn mail_of(item: &PimdirItem) -> &PimdirMailSummary {
    match &item.summary {
        Some(PimdirSummary::Mail(mail)) => mail,
        other => panic!("item {} has no mail summary: {other:?}", item.seq),
    }
}

/// A plain RFC 5322 message: `id` without angle brackets, `parent` the Message-ID it answers.
/// `date` is RFC 5322 (`Sat, 01 Aug 2026 12:00:00 +0000`).
pub fn eml(id: &str, parent: Option<&str>, from: &str, subject: &str, date: &str) -> Vec<u8> {
    let mut out = format!("Message-ID: <{id}>\r\n");
    if let Some(parent) = parent {
        out.push_str(&format!(
            "In-Reply-To: <{parent}>\r\nReferences: <{parent}>\r\n"
        ));
    }
    out.push_str(&format!(
        "From: {from}\r\nTo: Rail <me@example.org>\r\nSubject: {subject}\r\nDate: {date}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nHello from {id}.\r\n"
    ));
    out.into_bytes()
}
