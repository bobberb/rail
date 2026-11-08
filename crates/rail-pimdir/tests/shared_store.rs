//! One store holding several accounts, the layout Rail uses (docs/design/pimdir-port.md,
//! decision 4): each account's collections grouped under it, a unified inbox over all of them,
//! a reader that runs while the owner holds the store, and the one way two accounts collide.

use io_pimdir::client::PimdirStore;
use io_pimdir::client::reader::PimdirMailFilter;
use io_pimdir::client::reader::PimdirReader;
use rail_pimdir::fixture::{Fixture, Mailbox, Message, eml, mail_of};

/// Two accounts, one inbox each, each synced by a source named for its account.
fn two_accounts(dir: &std::path::Path) -> (Fixture, Mailbox, Mailbox) {
    let fixture = Fixture::create(dir).unwrap();
    let work = fixture
        .mailbox("work", "work-imap", "INBOX", Some("inbox"))
        .unwrap();
    let home = fixture
        .mailbox("home", "home-gmail", "INBOX", Some("inbox"))
        .unwrap();
    fixture
        .land(
            &work,
            &[
                Message::new(
                    "1",
                    eml(
                        "w1@x",
                        None,
                        "Ann <ann@corp.example>",
                        "Standup",
                        "Mon, 03 Aug 2026 09:00:00 +0000",
                    ),
                ),
                Message::new(
                    "2",
                    eml(
                        "w2@x",
                        None,
                        "Ann <ann@corp.example>",
                        "Review",
                        "Mon, 03 Aug 2026 11:00:00 +0000",
                    ),
                ),
            ],
        )
        .unwrap();
    fixture
        .land(
            &home,
            &[Message::new(
                "a",
                eml(
                    "h1@x",
                    None,
                    "Mum <mum@home.example>",
                    "Sunday",
                    "Mon, 03 Aug 2026 10:00:00 +0000",
                ),
            )],
        )
        .unwrap();
    (fixture, work, home)
}

fn subjects(reader: &PimdirReader, collections: &[&str]) -> Vec<String> {
    reader
        .list_mail_page_filtered(collections, PimdirMailFilter::default(), None, None, 50)
        .unwrap()
        .iter()
        .map(|entry| mail_of(&entry.item).subject.clone())
        .collect()
}

#[test]
fn each_accounts_collections_are_grouped_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, work, home) = two_accounts(dir.path());
    let reader = PimdirReader::open(fixture.dir()).unwrap();

    assert_eq!(reader.list_accounts().unwrap(), ["home", "work"]);
    let ids = |account| -> Vec<String> {
        reader
            .list_collections_by_account(Some(account))
            .unwrap()
            .into_iter()
            .map(|collection| collection.id)
            .collect()
    };
    assert_eq!(ids("work"), [work.id]);
    assert_eq!(ids("home"), [home.id]);
}

#[test]
fn the_unified_inbox_interleaves_accounts_by_date() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, work, home) = two_accounts(dir.path());
    let reader = PimdirReader::open(fixture.dir()).unwrap();

    assert_eq!(
        subjects(&reader, &[&work.id, &home.id]),
        ["Review", "Sunday", "Standup"]
    );
    assert_eq!(subjects(&reader, &[&home.id]), ["Sunday"]);
}

#[test]
fn a_reader_runs_while_the_owner_holds_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, work, _) = two_accounts(dir.path());

    // neverest mid-sync: the owner lock held, and a write transaction open on the database.
    let owner = PimdirStore::open(fixture.dir()).unwrap();
    let mut writer = rusqlite::Connection::open(fixture.dir().join("pimdir.db")).unwrap();
    let writing = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    writing
        .execute("UPDATE store_meta SET version = version", [])
        .unwrap();

    let reader = PimdirReader::open(fixture.dir()).expect("readers take no lock");
    assert_eq!(
        subjects(&reader, &[&work.id]),
        ["Review", "Standup"],
        "a reader reads the last commit while a write is in flight (WAL)"
    );
    writing.rollback().unwrap();
    drop(owner);
}

#[test]
fn two_accounts_naming_their_sources_alike_share_one_inbox() {
    // neverest names a collection `<source>/<mailbox>`, so this is what a shared store does
    // when two accounts both call their source `imap`: one collection, owned by whichever
    // account declared it first. Rail's doctor must refuse such a config (phase 2).
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::create(dir.path()).unwrap();
    let work = fixture
        .mailbox("work", "imap", "INBOX", Some("inbox"))
        .unwrap();
    let home = fixture
        .mailbox("home", "imap", "INBOX", Some("inbox"))
        .unwrap();

    assert_eq!(work.id, home.id);
    let reader = PimdirReader::open(fixture.dir()).unwrap();
    assert_eq!(
        reader.collection_account("imap/INBOX").unwrap(),
        Some(Some("work".into()))
    );
    assert!(
        reader
            .list_collections_by_account(Some("home"))
            .unwrap()
            .is_empty()
    );
}
