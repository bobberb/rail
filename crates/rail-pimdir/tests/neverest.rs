//! Reads a store that a real neverest synced: the second half of
//! scripts/neverest-shared-store.sh, which syncs alice@rail.test and bob@rail.test into one
//! store and points `RAIL_NEVEREST_STORE` at it. Ignored otherwise, as it needs that store.

use io_pimdir::client::reader::{PimdirMailFilter, PimdirReader};
use io_pimdir::client::search::{PimdirIndex, PimdirSearchOptions};
use io_pimdir::search::query::PimdirQuery;
use rail_pimdir::fixture::{Fixture, mail_of};

#[test]
#[ignore = "needs a neverest-synced store: run scripts/neverest-shared-store.sh"]
fn two_neverest_accounts_share_one_store() {
    let dir = std::env::var("RAIL_NEVEREST_STORE")
        .expect("RAIL_NEVEREST_STORE names the store scripts/neverest-shared-store.sh synced");
    let reader = PimdirReader::open(&dir).expect("Rail opens the store neverest wrote");

    assert_eq!(reader.list_accounts().unwrap(), ["alice", "bob"]);
    for account in ["alice", "bob"] {
        let ids: Vec<_> = reader
            .list_collections_by_account(Some(account))
            .unwrap()
            .into_iter()
            .map(|collection| collection.id)
            .collect();
        assert_eq!(
            ids,
            [format!("{account}-imap/INBOX")],
            "{account}'s collections"
        );
    }

    let inboxes = ["alice-imap/INBOX", "bob-imap/INBOX"];
    let subjects: Vec<_> = reader
        .list_mail_page_filtered(&inboxes, PimdirMailFilter::default(), None, None, 50)
        .unwrap()
        .iter()
        .map(|entry| mail_of(&entry.item).subject.clone())
        .collect();
    assert_eq!(
        subjects,
        ["Lunch?", "Re: Plans", "Plans"],
        "one unified inbox, newest first"
    );

    // neverest refreshes the index after a run, so threads are there for Rail to read.
    let index = PimdirIndex::open_query(&dir).expect("neverest built the search index");
    let listing = index
        .threads(
            &PimdirQuery::parse("kind:mail").unwrap(),
            &PimdirSearchOptions::default(),
        )
        .unwrap();
    let mut threads: Vec<_> = listing
        .threads
        .iter()
        .map(|thread| (thread.account.clone().unwrap_or_default(), thread.count))
        .collect();
    threads.sort();
    assert_eq!(threads, [("alice".to_owned(), 2), ("bob".to_owned(), 1)]);

    // The fixture's capability rows are neverest's: a fixture of the same accounts declares
    // what this real run declared, so producers in later phases test the path a real store
    // takes.
    let scratch = tempfile::tempdir().unwrap();
    let fixture = Fixture::create(scratch.path()).unwrap();
    let modelled = PimdirReader::open(fixture.dir()).unwrap();
    for account in ["alice", "bob"] {
        let source = format!("{account}-imap");
        let id = format!("{source}/INBOX");
        let role = reader
            .list_collections_by_account(Some(account))
            .unwrap()
            .into_iter()
            .find(|collection| collection.id == id)
            .unwrap()
            .role;
        fixture
            .mailbox(account, &source, "INBOX", role.as_deref())
            .unwrap();
        assert_eq!(
            modelled.capabilities(&id).unwrap(),
            reader.capabilities(&id).unwrap(),
            "{account}: the fixture declares what neverest declared",
        );
        assert_eq!(
            modelled.collection_sources(&id).unwrap(),
            reader.collection_sources(&id).unwrap(),
            "{account}: the same sources sync the inbox",
        );
    }
    eprintln!(
        "read {} messages in {} threads from {dir}",
        subjects.len(),
        threads.len()
    );
}
