//! The latency budget of docs/design/pimdir-port.md, outcome 3: on a 10,000-message store the
//! unified inbox lists in under 50 ms at p95. Rail's promise is that the interface never waits
//! on the network, and these are the reads that have to keep it.
//!
//! Rail's inbox is a thread list, so the threaded listing is held to the budget too. It misses
//! it today (constraint 6 of the plan), so this test fails until phase 2 pages threads within
//! budget; the failure names every read over budget.
//!
//! Building the store takes a while, so this is not a gate test. Run it in release:
//!
//! ```sh
//! cargo test -p rail-pimdir --release --test budget -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use io_pimdir::client::reader::{PimdirMailFilter, PimdirReader};
use io_pimdir::client::search::{PimdirIndex, PimdirSearchOptions};
use io_pimdir::search::query::PimdirQuery;
use rail_pimdir::fixture::{Fixture, Message, eml};

/// The list budget at p95.
const BUDGET: Duration = Duration::from_millis(50);
/// Messages in the store, split across two accounts.
const MESSAGES: usize = 10_000;
/// One page of the list, about a tall terminal's worth.
const PAGE: usize = 50;

/// The `q`th quantile of `samples` (0 < q <= 1), nearest rank.
fn quantile(samples: &mut [Duration], q: f64) -> Duration {
    samples.sort_unstable();
    let rank = (q * samples.len() as f64).ceil() as usize;
    samples[rank.clamp(1, samples.len()) - 1]
}

/// Runs `f` `runs` times and answers (p50, p95).
fn time(runs: usize, mut f: impl FnMut()) -> (Duration, Duration) {
    f(); // warm the page cache and the statement cache
    let mut samples: Vec<_> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .collect();
    (quantile(&mut samples, 0.50), quantile(&mut samples, 0.95))
}

/// Messages in every thread of the bench store.
const THREAD: usize = 4;

/// Two accounts of 5,000 messages each, one a minute over about four days, the two accounts'
/// minutes coinciding. Every fourth message starts a thread and the next three reply in a
/// chain, so every thread holds exactly [`THREAD`] messages.
fn build(dir: &std::path::Path) -> (Fixture, Vec<String>) {
    let fixture = Fixture::create(dir).unwrap();
    let mut inboxes = Vec::new();
    for account in ["work", "home"] {
        let inbox = fixture
            .mailbox(account, &format!("{account}-imap"), "INBOX", Some("inbox"))
            .unwrap();
        let per = MESSAGES / 2;
        for chunk in (0..per).collect::<Vec<_>>().chunks(500) {
            let batch: Vec<_> = chunk
                .iter()
                .map(|&n| {
                    let id = format!("{account}-{n}@x");
                    let parent =
                        (!n.is_multiple_of(THREAD)).then(|| format!("{account}-{}@x", n - 1));
                    let day = 1 + (n / 1440) % 28;
                    let (hour, minute) = ((n / 60) % 24, n % 60);
                    let date = format!("{day:02} Jul 2026 {hour:02}:{minute:02}:00 +0000");
                    let body = eml(
                        &id,
                        parent.as_deref(),
                        "Ann <ann@example.org>",
                        &format!("Subject {n}"),
                        &date,
                    );
                    Message::new(n.to_string(), body)
                })
                .collect();
            fixture.land(&inbox, &batch).unwrap();
        }
        inboxes.push(inbox.id);
    }
    fixture.index().unwrap();
    (fixture, inboxes)
}

#[test]
#[ignore = "builds a 10,000-message store; run in release with --ignored"]
fn the_unified_inbox_lists_within_budget() {
    let dir = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let (fixture, inboxes) = build(dir.path());
    eprintln!(
        "built {MESSAGES} messages and the index in {:?}",
        start.elapsed()
    );

    let reader = PimdirReader::open(fixture.dir()).unwrap();
    let filter = PimdirMailFilter::default;
    let first_page = time(200, || {
        let page = reader
            .list_mail_page_filtered(&inboxes, filter(), None, None, PAGE)
            .unwrap();
        assert_eq!(page.len(), PAGE);
    });

    // A page deep in the list, resumed from a cursor as scrolling does.
    let mut cursor = None;
    for _ in 0..100 {
        let page = reader
            .list_mail_page_filtered(&inboxes, filter(), None, cursor.as_ref(), PAGE)
            .unwrap();
        cursor = page.last().map(|entry| entry.cursor());
    }
    assert!(cursor.is_some(), "100 full pages before the timed one");
    let deep_page = time(200, || {
        let page = reader
            .list_mail_page_filtered(&inboxes, filter(), None, cursor.as_ref(), PAGE)
            .unwrap();
        assert_eq!(page.len(), PAGE, "page 101 of 200 is full");
    });

    let index = PimdirIndex::open_query(fixture.dir()).unwrap();
    let all_mail = PimdirQuery::parse("kind:mail").unwrap();
    let options = PimdirSearchOptions {
        limit: PAGE,
        ..Default::default()
    };
    let threaded = time(20, || {
        let listing = index.threads(&all_mail, &options).unwrap();
        assert_eq!(listing.threads.len(), PAGE);
    });
    // The thread's own account: which account lists first depends on how ties between the two
    // accounts' identical dates break, and a wrong one answers `None` fast.
    let first = index
        .threads(&all_mail, &options)
        .unwrap()
        .threads
        .remove(0);
    let (account, seq) = (first.account.clone(), first.hits[0].seq);
    let open_thread = time(200, || {
        let thread = index
            .thread(account.as_deref(), seq)
            .unwrap()
            .expect("the listed thread opens in its own account");
        assert_eq!(thread.hits.len(), THREAD);
    });

    let reads = [
        ("unified list, first page", first_page),
        ("unified list, page 101", deep_page),
        ("threaded listing, first page", threaded),
        ("open one thread", open_thread),
    ];
    eprintln!("read                             p50        p95");
    for (name, (p50, p95)) in reads {
        eprintln!("{name:<30} {p50:>10.2?} {p95:>10.2?}");
    }

    let over: Vec<_> = reads
        .iter()
        .filter(|(_, (_, p95))| *p95 >= BUDGET)
        .map(|(name, (_, p95))| format!("{name}: p95 {p95:.2?}"))
        .collect();
    assert!(
        over.is_empty(),
        "over the {BUDGET:?} budget (constraint 6 of docs/design/pimdir-port.md): {}",
        over.join("; ")
    );
}
