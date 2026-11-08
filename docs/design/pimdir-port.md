# Port plan: Rail's UI on neverest + io-pimdir

Status: **in progress**, 2026-11-07. Phase 0 is done; the UI still runs on rail-core.

## Goal

Keep Rail's terminal UI (`crates/rail/src/tui`, about 57k lines) and drop Rail's own
backend: the daemon, sync engine, Gmail and Microsoft 365 providers, OAuth, onboarding,
SQLite store and MCP server (`crates/rail-core`, about 98k lines, plus `crates/rail/src/cmd`).
Mail comes from the Pimalaya stack instead:

- **neverest** syncs every account into a local **pimdir** store and applies queued actions.
- **io-pimdir** is linked into Rail. Rail reads the store and queues actions through it.
- **himalaya** keeps working on the same store through its `pimdir` backend, so the CLI and
  the TUI see the same mail.

### Measurable outcome

The port is done when all of these hold:

1. `rail` opens on a neverest-synced pimdir store and list, open thread, archive, trash,
   star, read/unread, move, label, reply, forward, new message, save draft and send all
   work, each covered by a test against a fixture store.
2. `crates/rail-core` no longer holds a daemon, sync, provider, auth, onboarding or store
   module. `cargo tree -p rail` shows no `reqwest`, `oauth2`, `keyring` or `rmcp`.
3. Listing the unified inbox on a 10,000-message fixture store takes under 50 ms at p95,
   measured by a bench in the test suite. (Rail's promise is that the interface never waits
   on the network, and the port keeps it.)
4. The TUI's existing test suite passes. Tests that drove `FakeDaemon` now drive a
   fixture store.

## Why this backend

Decided 2026-11-07, after surveying the pimalaya org:

| Option | Verdict |
|---|---|
| Link himalaya | Impossible. It ships no library target (`himalaya/src/main.rs:12-15`). |
| io-email (the unified client library) | Abandoned: 0.1.0, 135 recent downloads, pins io-* 0.1 while they are at 0.8. |
| email-lib (pimalaya/core) | The v1 generation himalaya v2 left behind. |
| io-* protocol crates directly | Brings back the backend we are cutting, about 3k lines of glue per backend set. |
| `himalaya --json repl` subprocess | Works, but every list is a network round trip. Rail loses its local store. |
| **neverest + io-pimdir** | **Chosen.** It is Rail's own design (a sync process fills a local store, the UI reads it) as a maintained standard. |

How Rail's pieces map onto pimdir:

| Rail today | Replacement | Source |
|---|---|---|
| daemon + sync + providers | `neverest sync` (IMAP, JMAP, Gmail, MS Graph) | neverest `Cargo.toml` |
| `StoreReader` (SQLite reads) | `PimdirReader`: lock-free reader, mail cursors, summaries | io-pimdir `src/client/reader.rs:308,510,729` |
| thread view | `PimdirIndex::threads` / `thread` (In-Reply-To, then References; never by subject) | io-pimdir `src/client/search.rs:667,741`, `src/search/thread.rs:1-14` |
| filter, saved searches | `PimdirIndex::search` + the pimdir query language | io-pimdir `src/client/search.rs:635`, pimdir `SEARCH.md` |
| `DaemonLink::apply` (archive, trash, star, read, move, label) | `PimdirProducer::enqueue`: `move`, `remove`, `set-flags`, `copy` | pimdir `STORAGE.md` §15.3 |
| drafts | `add` / `update` actions into the Drafts collection | `STORAGE.md` §15.3 |
| send | the `mail.submit` intent (`PimdirSubmit`) | io-pimdir `src/intent.rs:222` |
| optimistic UI after an action | `PimdirReader::with_pending()` overlays queued actions | `reader.rs:539`, `STORAGE.md` §15.4 |
| requery when the store changes | `PimdirReader::change_cursor()`, the change feed | `reader.rs:1274`, `STORAGE.md` §4.5 |
| outbox, failed actions, retry | parked actions (`pending_actions`, `action_status`) | `producer.rs:492,502`, `STORAGE.md` §15.2 |
| address completion | the address cursor | `reader.rs:180` |
| attachments, inline pictures | `PimdirReader::blobs()` | `reader.rs:550` |

## Constraints that shape the design

1. **A store opens under exactly one spec draft.** io-pimdir 0.9.0 on crates.io is
   storage-draft-08, but neverest and himalaya both pin git rev
   `5c18a026b9c85b470984192452c2aa66e060a3fe` (storage-draft-09). Rail pins that same rev.
   `tests/pin.rs` holds the pin to `Cargo.lock` and to the draft a store is stamped with,
   and the CI job `pimdir-pin` (`scripts/check-pimdir-pin.sh`) fails when neverest's or
   himalaya's master pins another rev.
2. **Rail is a producer, not the owner.** Only the owner (neverest) mutates the store.
   Rail can queue an action but cannot cancel it: `cancel_action` is an owner write
   (`STORAGE.md` §15.5). So **undo send and undo action hold the action inside Rail for the
   grace window** (`ops::UNDO_GRACE`, 10 s today) and only enqueue it when the window
   closes. Quitting Rail mid-window enqueues what is held, so nothing is lost.
3. **neverest syncs once per run.** There is no daemon mode. Rail's sync driver runs
   `neverest sync --json` on `Ctrl-r`, on a timer (default 2 min, configurable), and right
   after it enqueues actions. It reads the JSON report for the status line. Only one run at a
   time; neverest takes a store lock (`neverest/src/cli/sync.rs:38`) and Rail does not
   stack runs behind it.
4. **The search index is experimental.** Its API may change in a minor release
   (io-pimdir README). Threads and search sit behind one module in `rail-pimdir`, so a
   break is fixed in one place. If the index is missing or out of date, the list falls back
   to one row per message from `list_mail_page_filtered`.
5. **Rail's UI is keyed by `i64`** (account_id, thread_id, message_id) across about 45
   import sites. Pimdir keys are an account string, a collection id plus `seq`, a
   `link_id`, and a thread name. An interner in `rail-pimdir` maps them to session-scoped
   `i64`s, so the screens don't change type. The ids are never persisted; anything Rail
   stores (snoozes, saved searches) uses pimdir's own keys.
6. **The threaded listing is too slow to page the inbox.** Measured in phase 0 on 10,000
   messages (`crates/rail-pimdir/tests/budget.rs`, release): the flat unified list pages in
   0.82 ms p95 and opening one thread takes 0.89 ms p95, but `PimdirIndex::threads` takes
   475 ms p95 for its first page, 9.5× the budget. It collects every match before paging
   (`found(..., None)` then one `HIT_THREAD` lookup per hit, io-pimdir
   `src/client/search.rs:667-700`), so it grows with the mailbox, not with the page. Phase 2
   has to answer this before the thread list is built on it: candidates are collapsing the
   flat page into threads in `rail-pimdir` with a batched thread-id lookup, or a paged
   `threads` upstream (deferred, like all upstream work).
7. **In a shared store, every account's sources need distinct names, and neverest's defaults
   don't give them.** neverest names a collection `<source>/<mailbox>` (`hub_id`,
   `neverest/src/offline/driver.rs:129,1469`), and its short config form (`imap.server = …`
   under the account), which `neverest configure` writes (`src/wizard/discover.rs:11-12`),
   names the source after its protocol (`src/config.rs:475`). So two wizard-made IMAP
   accounts pointed at one store would both sync `imap/INBOX`: one collection, owned by
   whichever declared it first (`crates/rail-pimdir/tests/shared_store.rs`). It is worse
   than a shared inbox: sync state is keyed by collection and source, not account
   (`src/offline/storage.rs:174-190`), so the second account would reconcile its server
   against the first account's checkpoint and placements, likely tombstoning or duplicating
   mail. The wizard also writes no `store.root`, so a shared store always needs a config
   written for it. Rail therefore owns that config: phase 2 adds a command that writes the
   neverest config for the shared store with `sources.<account>-<protocol>` names, and Rail's
   doctor refuses to sync a shared store where two accounts' source names meet.
8. **The workspace moved to rusqlite 0.41 and Rust 1.89.** io-pimdir needs both, and
   `libsqlite3-sys` links one SQLite per build, so rail-core moved with it in phase 0
   (its 1,509 tests pass unchanged).

## Target layout

```
crates/
  rail-model/    types the UI shares with its data layer: view rows (ThreadDetail, list
                 rows, LabelChoices), action and draft requests and responses, config
                 (themes, keys, icons, saved searches), paths, ErrorClass, the filter
                 grammar, the HTML-to-text renderer. No I/O.
  rail-pimdir/   the only crate that touches io-pimdir and neverest: reads, the queue,
                 the hold-for-undo buffer, the id interner, the sync driver, snooze.
                 Its own tests, run against fixture stores.
  rail/          the binary and the TUI. Depends on rail-model, and on rail-pimdir only
                 through tui/data/.
```

`rail-core` is gone at the end. Each crate keeps its own tests, so a change in
`rail-pimdir` is checked without the 57k-line TUI suite.

## What happens to each feature

| Feature | Fate |
|---|---|
| unified inbox, per-account views | **Kept.** One store all accounts share (decision 4): the unified inbox lists every account's collections, a per-account view one account's. |
| threads | **Kept**, from the pimdir index. |
| filter as you type, saved searches | **Kept.** The filter compiles to a pimdir query. Saved searches stay in Rail's `config.toml`. |
| archive, trash, star, read, move, labels | **Kept**, as queued actions. Gmail labels: add = `copy`, remove = `remove` of that placement. |
| undo action, undo send | **Kept**, held in Rail for the grace window (constraint 2). |
| drafts | **Kept**, as `add`/`update` in the account's Drafts collection. |
| send | **Kept**, through `mail.submit`. neverest performs it over SMTP, Gmail or Graph. |
| outbox, retry | **Becomes** a view of parked actions. Retry re-enqueues the action. |
| snooze | **Kept, Rail-local.** State lives in a small Rail-owned file keyed by account plus link_id. On wake, Rail queues a `move` back to the inbox. Upstream work (a pimdir intent) is deferred. |
| sync status line | **Kept**, read from neverest's `--json` report. |
| HTML reading, inline pictures, links | **Kept.** The renderer moves to `rail-model`; bodies come from blobs. |
| attachments open and save | **Kept**, from blobs. |
| account setup (`rail account add`, onboarding) | **Dropped.** Use `neverest configure`; `rail` points at its stores. |
| `rail daemon`, login autostart | **Dropped**, replaced by the sync driver (constraint 3). |
| OAuth, keyring | **Dropped**; neverest owns credentials. |
| MCP server (`rail mcp`) | **Dropped.** himalaya ships its own. |
| `rail doctor` | **Reduced** to: neverest found and runnable, store opens, draft pin matches. |

## Phases

Each phase leaves the tree building with its tests green, and each lands as its own commit.

0. **Pin and fixtures. Done** (2026-11-07). io-pimdir pinned at `5c18a02` with `client`
   and `search` (workspace `Cargo.toml`), restated as `rail_pimdir::IO_PIMDIR_REV` and held
   to `Cargo.lock` and the library's `FORMAT_VERSION` by `tests/pin.rs`;
   `scripts/check-pimdir-pin.sh` compares it with neverest's and himalaya's master. The
   fixture builder (`rail_pimdir::fixture`, feature `fixture`) leaves a store in the state a
   neverest IMAP sync does, for everything a reader or producer sees: each source's
   capabilities declared as neverest declares them (producers branch on it, STORAGE §15.6),
   each collection's syncing row as `register_syncing` records it, and each message as a
   reconciled, hydrated placement with its `base`, landed through the source write path
   (not `put_item`, which refuses mail). Checkpoints, rounds and coverage are not mirrored;
   only the sync engine reads them. Tests cover paging, summaries, flags, threading by
   parent (not subject), declared capabilities, a Gmail label as a second placement, and the
   shared store (decision 4). `tests/neverest.rs` compares a fixture's capability rows with
   a real run's. The 10k bench is `tests/budget.rs` (constraint 6 has the numbers). The
   fixtures are not run against pimdir's vectors: they go through the write path whose
   output io-pimdir's own suite checks against them, so the vectors would test io-pimdir,
   not Rail.
1. **Extract `rail-model`.** Move the rail-core types the TUI imports into the new crate.
   This is mechanical: the TUI's imports change and its behavior doesn't. Gate: the full
   TUI suite passes unchanged.
2. **Build `rail-pimdir`** behind the API the TUI already calls (the `StoreReader` and
   `DaemonLink` method list in `crates/rail/src/tui/data/`): the reader, interner, index
   wrapper, producer, hold-for-undo buffer and sync driver. Unit tests run against
   fixtures; the sync driver is tested against a stub `neverest` on `PATH`.
3. **Swap the TUI's data layer.** `tui/data/` calls `rail-pimdir`. `FakeDaemon` tests
   become fixture-store tests. Gate: the TUI suite passes.
4. **Compose and send.** Draft `add`/`update`, `mail.submit`, undo send, the parked-action
   outbox.
5. **Snooze and saved searches** on the new keys.
6. **Cut.** Delete the rail-core backend modules, `crates/rail/src/cmd` (except doctor),
   the MCP server and their tests. Update README, `docs/`, CHANGELOG and `deny.toml`.
   Gate: outcomes 2 and 3 hold.
7. **End to end, by hand.** A real neverest run against a test account: sync, archive in
   Rail, sync, see it archived on the server. Record the run in this doc.
8. **Ship.** Each phase is a PR on `bobberb/rail` (decision 3). Nothing goes to
   `vitosans/rail`.

## Verification log

What was checked for this plan, against which source (repos cloned 2026-11-07):

| Claim | Checked against |
|---|---|
| himalaya has no library target | `himalaya/src/main.rs:12-15`; no `src/lib.rs` |
| io-email is stale | crates.io API: 0.1.0, updated 2026-06-05, 135 recent downloads |
| io-pimdir is current | crates.io API: 0.9.0, updated 2026-11-04 |
| neverest and himalaya pin io-pimdir `5c18a02` | `neverest/Cargo.toml:86`, `himalaya/Cargo.toml:96` |
| queue actions and their payloads | pimdir `STORAGE.md` §15.3 |
| producers cannot cancel | pimdir `STORAGE.md` §15.5 |
| threads by In-Reply-To/References, never subject | io-pimdir `src/search/thread.rs:1-14` |
| mail summary fields (subject, sender, date, size, attachment, from/to/cc/bcc, in_reply_to) | io-pimdir `src/summary/mail.rs:47-74` |
| several accounts share one store | io-pimdir `tests/multi_account.rs`; neverest `src/offline/driver.rs:214-224` ("a store shared by two accounts tells whose is whose") and `:323` ("they may share a `store.root`"); Rail `crates/rail-pimdir/tests/shared_store.rs` |
| a reader opens while the owner holds the store | `crates/rail-pimdir/tests/shared_store.rs` |
| collections are `<source>/<mailbox>` | neverest `src/offline/driver.rs:129,1469` |
| one message in two mailboxes keeps one `seq` and body | `crates/rail-pimdir/tests/fixture.rs` |
| **outcome 3 (50 ms p95 inbox)** | **not met.** Rail's inbox is a thread list, and the threaded listing takes 475 ms p95 (constraint 6). The flat list (0.82 ms p95 first page and page 101) and opening a thread (0.89 ms) are within it. `tests/budget.rs` asserts all four and fails until phase 2. |
| replies thread by parent, not subject | `crates/rail-pimdir/tests/fixture.rs` (a renamed reply joins, a "Re:" look-alike doesn't) |
| rail-core runs on rusqlite 0.41 | `cargo test -p rail-core`: 1,509 passed |

Not verified yet:

- **Two real neverest accounts syncing into one `store.root`.**
  `scripts/neverest-shared-store.sh` does it end to end (Stalwart in a container, two users,
  then `crates/rail-pimdir/tests/neverest.rs` reads the store and compares a fixture's
  capability rows with the run's), but it could not run in the session that wrote it: the
  sandbox there blocks podman from publishing ports. The source evidence above is why
  decision 4 stands; the script turns it into a run.
- **Gmail labels as `copy`/`remove`.** The store side is settled (a label is a second
  placement of one `seq`), but how neverest's Gmail source maps labels needs a Gmail
  account. Settled in phase 7.
- **neverest `--json` report schema** for the status line. Settled in phase 2 with
  `neverest json-schema`.

## Decisions

Recorded 2026-11-07:

1. **Repo:** `bobberb/rail`, a GitHub fork of `vitosans/rail`.
2. **Sync:** Rail spawns `neverest sync` itself (constraint 3). No timer or carillon.
3. **Landing:** one PR per phase on `bobberb/rail`, each merged before the next starts.
4. **Stores:** one shared store. Every neverest account points `store.root` at the same
   directory, as Rail's one `rail.db` holds every account today
   (`crates/rail-core/src/paths.rs:130`). Collections are grouped by account, so the
   unified inbox and cross-account search come from pimdir directly; account syncs take
   turns on the owner lock. Sources need distinct names across accounts (constraint 7).

## Open questions

1. **How to page the threaded inbox** within budget (constraint 6). Decided in phase 2,
   with numbers from `tests/budget.rs`.
