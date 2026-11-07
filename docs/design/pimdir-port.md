# Port plan: Rail's UI on neverest + io-pimdir

Status: **draft**, 2026-11-07. Nothing in this plan is built yet.

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
   `5c18a026b9c85b470984192452c2aa66e060a3fe` (storage-draft-09). Rail pins that same rev,
   and a CI check fails when Rail's pin differs from the neverest Rail is tested against.
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
| unified inbox, per-account views | **Kept.** One store per neverest account (an account is one store, per neverest's config), unioned in `rail-pimdir`. |
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

0. **Pin and fixtures.** Add io-pimdir at rev `5c18a02` with `client` and `search`.
   Write a fixture builder that uses the owner handle (`PimdirStore::put_item`, the same
   path neverest uses) to create small stores from `.eml` files: threads, Gmail labels in
   several collections, a 10k-message store for the bench. Check the fixtures against
   pimdir's own vectors (`pimdir/vectors`).
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
8. **Ship.** Create the new repo under `bobberb` (name to decide), push, open the PR there.
   Nothing goes to `vitosans/rail`.

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

Not verified yet:

- **Gmail labels as `copy`/`remove`.** Depends on how neverest's Gmail source represents
  labels (collections or flags). Settled in phase 0 by syncing a fixture through neverest's
  Gmail tests.
- **neverest `--json` report schema** for the status line. Settled in phase 2 with
  `neverest json-schema`.
- **The 50 ms p95 budget.** Measured in phase 0 on the 10k fixture; if pimdir can't meet
  it, the budget is revisited with numbers, not dropped.

## Open questions

1. The new repo's name under `bobberb`.
2. One neverest account per mail account (several stores, Rail unions them), or one
   neverest account with several sources (one store)? The plan assumes the first because it
   matches neverest's defaults.
3. Should Rail spawn neverest itself (plan default), or should a systemd timer / carillon
   own sync, with Rail only watching the change feed?
