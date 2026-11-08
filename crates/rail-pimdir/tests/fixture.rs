//! What Rail's UI needs from a store, read back from a fixture: summaries, paging, flags,
//! threads, and a Gmail label as a second placement of one message.

use io_pimdir::client::reader::PimdirMailFilter;
use io_pimdir::client::reader::PimdirReader;
use io_pimdir::client::search::{PimdirIndex, PimdirSearchOptions};
use io_pimdir::search::query::PimdirQuery;
use rail_pimdir::fixture::{Fixture, Mailbox, Message, eml, mail_of};

const ANN: &str = "Ann <ann@example.org>";
const BOB: &str = "Bob <bob@example.org>";

/// One account: a thread of three (a root, a reply, a reply to the reply), and one message on
/// its own, all in the inbox; the root read and starred.
fn seeded(dir: &std::path::Path) -> (Fixture, Mailbox) {
    let fixture = Fixture::create(dir).unwrap();
    let inbox = fixture
        .mailbox("me", "gmail", "INBOX", Some("inbox"))
        .unwrap();
    fixture
        .land(
            &inbox,
            &[
                Message::new(
                    "1",
                    eml(
                        "root@x",
                        None,
                        ANN,
                        "Plans",
                        "Sat, 01 Aug 2026 09:00:00 +0000",
                    ),
                )
                .flags(&["\\Seen", "\\Flagged"]),
                Message::new(
                    "2",
                    eml(
                        "reply@x",
                        Some("root@x"),
                        BOB,
                        "Re: Plans",
                        "Sat, 01 Aug 2026 10:00:00 +0000",
                    ),
                ),
                Message::new(
                    "3",
                    eml(
                        "again@x",
                        Some("reply@x"),
                        ANN,
                        "Re: Plans",
                        "Sat, 01 Aug 2026 11:00:00 +0000",
                    ),
                ),
                Message::new(
                    "4",
                    eml(
                        "alone@x",
                        None,
                        BOB,
                        "Lunch?",
                        "Sat, 01 Aug 2026 12:00:00 +0000",
                    ),
                ),
            ],
        )
        .unwrap();
    (fixture, inbox)
}

#[test]
fn a_page_lists_newest_first_with_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, inbox) = seeded(dir.path());
    let reader = PimdirReader::open(fixture.dir()).unwrap();

    let page = reader
        .list_mail_page_filtered(&[&inbox.id], PimdirMailFilter::default(), None, None, 10)
        .unwrap();

    let subjects: Vec<_> = page
        .iter()
        .map(|entry| mail_of(&entry.item).subject.clone())
        .collect();
    assert_eq!(subjects, ["Lunch?", "Re: Plans", "Re: Plans", "Plans"]);

    let lunch = mail_of(&page[0].item);
    assert_eq!(lunch.sender.as_deref(), Some("bob@example.org"));
    assert_eq!(lunch.sender_name.as_deref(), Some("Bob"));
}

#[test]
fn a_cursor_resumes_the_next_page() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, inbox) = seeded(dir.path());
    let reader = PimdirReader::open(fixture.dir()).unwrap();
    let filter = PimdirMailFilter::default;

    let first = reader
        .list_mail_page_filtered(&[&inbox.id], filter(), None, None, 2)
        .unwrap();
    let cursor = first.last().unwrap().cursor();
    let second = reader
        .list_mail_page_filtered(&[&inbox.id], filter(), None, Some(&cursor), 2)
        .unwrap();

    let seqs = |page: &[io_pimdir::client::reader::PimdirMailEntry]| {
        page.iter().map(|entry| entry.item.seq).collect::<Vec<_>>()
    };
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert!(
        seqs(&first).iter().all(|seq| !seqs(&second).contains(seq)),
        "pages overlap"
    );
}

#[test]
fn flags_filter_unread_and_mark_stars() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, inbox) = seeded(dir.path());
    let reader = PimdirReader::open(fixture.dir()).unwrap();

    let unread = reader
        .list_mail_page_filtered(
            &[&inbox.id],
            PimdirMailFilter {
                seen: Some(false),
                attachment: None,
            },
            None,
            None,
            10,
        )
        .unwrap();
    assert_eq!(unread.len(), 3, "the root alone is \\Seen");

    let all = reader
        .list_mail_page_filtered(&[&inbox.id], PimdirMailFilter::default(), None, None, 10)
        .unwrap();
    let starred: Vec<_> = all
        .iter()
        .filter(|entry| entry.item.flags.contains("\\Flagged"))
        .map(|entry| mail_of(&entry.item).subject.clone())
        .collect();
    assert_eq!(starred, ["Plans"]);
}

#[test]
fn replies_thread_by_their_parents_not_their_subjects() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, inbox) = seeded(dir.path());
    // A reply that changed the subject belongs to its parent's thread, and a message that only
    // looks like a reply (a "Re:" subject, no parent) belongs to none: subject matching would
    // get both wrong.
    fixture
        .land(
            &inbox,
            &[
                Message::new(
                    "5",
                    eml(
                        "renamed@x",
                        Some("again@x"),
                        BOB,
                        "Booked the room",
                        "Sat, 01 Aug 2026 13:00:00 +0000",
                    ),
                ),
                Message::new(
                    "6",
                    eml(
                        "lookalike@x",
                        None,
                        ANN,
                        "Re: Plans",
                        "Sat, 01 Aug 2026 14:00:00 +0000",
                    ),
                ),
            ],
        )
        .unwrap();
    fixture.index().unwrap();
    let index = PimdirIndex::open_query(fixture.dir()).unwrap();

    let listing = index
        .threads(
            &PimdirQuery::parse("kind:mail").unwrap(),
            &PimdirSearchOptions::default(),
        )
        .unwrap();

    let mut counts: Vec<_> = listing.threads.iter().map(|thread| thread.count).collect();
    counts.sort_unstable();
    assert_eq!(
        counts,
        [1, 1, 4],
        "Plans and its 3 replies; Lunch? alone; the look-alike alone"
    );

    let plans = listing
        .threads
        .iter()
        .find(|thread| thread.count == 4)
        .unwrap();
    let opened = index
        .thread(Some("me"), plans.hits[0].seq)
        .unwrap()
        .unwrap();
    let titles: Vec<_> = opened.hits.iter().map(|hit| hit.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Plans", "Re: Plans", "Re: Plans", "Booked the room"],
        "oldest first"
    );
}

#[test]
fn a_fixture_account_declares_what_an_imap_source_can_do() {
    // Producers branch on whether an account declared capabilities (STORAGE §15.6); a real
    // neverest run always declares, so a fixture must too.
    use io_pimdir::capability::PimdirSupport::{Full, None as Refused, Partial};

    let dir = tempfile::tempdir().unwrap();
    let (fixture, inbox) = seeded(dir.path());
    let label = fixture.mailbox("me", "gmail", "Travel", None).unwrap();
    let reader = PimdirReader::open(fixture.dir()).unwrap();

    let support = |collection: &str, name: &str| {
        let sources = reader.capabilities(collection).unwrap();
        assert_eq!(sources.len(), 1, "one source syncs {collection}");
        assert_eq!(sources[0].source, "gmail");
        let declared = sources[0].declared.as_ref().expect("the source declared");
        declared
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} declared"))
            .0
    };
    assert_eq!(support(&inbox.id, "mail.submit"), Full);
    assert_eq!(support(&inbox.id, "mail.message.move"), Full);
    assert_eq!(support(&inbox.id, "mail.flags.keywords"), Partial);
    assert_eq!(support(&inbox.id, "collection.delete"), Refused);
    assert_eq!(
        support(&inbox.id, "collection.rename"),
        Refused,
        "the inbox states a role"
    );
    assert_eq!(
        support(&label.id, "collection.rename"),
        Full,
        "a label does not"
    );
    assert_eq!(reader.collection_sources(&inbox.id).unwrap(), ["gmail"]);
}

#[test]
fn a_gmail_label_is_a_second_placement_of_one_message() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, inbox) = seeded(dir.path());
    let label = fixture.mailbox("me", "gmail", "Travel", None).unwrap();
    let body = eml(
        "trip@x",
        None,
        ANN,
        "Tickets",
        "Sun, 02 Aug 2026 08:00:00 +0000",
    );

    let in_inbox = fixture
        .land(&inbox, &[Message::new("10", body.clone())])
        .unwrap();
    let in_label = fixture.land(&label, &[Message::new("10", body)]).unwrap();

    assert_eq!(
        in_inbox, in_label,
        "one message, one seq and link id, wherever it is filed"
    );
    let reader = PimdirReader::open(fixture.dir()).unwrap();
    let placements = reader.link_placements(&in_inbox[0].link_id).unwrap();
    let collections: Vec<_> = placements
        .iter()
        .map(|placement| placement.collection.as_str())
        .collect();
    assert_eq!(collections, ["gmail/INBOX", "gmail/Travel"]);
    assert_eq!(
        placements[0].object, placements[1].object,
        "one body for both"
    );
}
