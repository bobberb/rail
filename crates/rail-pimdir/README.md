# rail-pimdir

Rail's data layer over a pimdir store. neverest syncs every account into one store and
applies the actions queued there; Rail links [io-pimdir] to read the store and to queue
actions. The plan is [docs/design/pimdir-port.md](../../docs/design/pimdir-port.md).

So far (phase 0) this crate holds the version pin and the fixture builder later phases test
against.

## The pin

A store opens under exactly one pimdir spec draft and is never migrated, so Rail must read
stores with the same io-pimdir as the neverest that writes them. The workspace pins git rev
`5c18a02` (storage-draft-09), the rev neverest and himalaya pin.

- `tests/pin.rs` fails when `Cargo.lock` locks another rev than `IO_PIMDIR_REV`, or when
  `STORAGE_DRAFT` is not the library's `FORMAT_VERSION`.
- `scripts/check-pimdir-pin.sh` compares the pin with neverest's and himalaya's master (needs
  network). Run it before bumping io-pimdir.

To move the pin: change the rev in the workspace `Cargo.toml` and `IO_PIMDIR_REV`, update
`STORAGE_DRAFT` if the draft moved, and run the checks below.

## Fixtures

With the `fixture` feature, `rail_pimdir::fixture` builds stores in the state a neverest IMAP
sync leaves them, for everything a reader or a producer sees: the source's declared
capabilities, the collection's syncing row, and each message as a reconciled placement with
its summary derived from the body. `tests/neverest.rs` checks the capability rows against a
real run. Collections are `<source>/<mailbox>`; in a shared store each account's sources need
distinct names.

```rust
let fixture = Fixture::create(dir)?;
let inbox = fixture.mailbox("work", "work-imap", "INBOX", Some("inbox"))?;
fixture.land(&inbox, &[Message::new("1", eml("a@x", None, "Ann <ann@x>", "Hi", date))])?;
fixture.index()?; // threads and search
```

## Checks

| What | Command | Time |
|---|---|---|
| gate tests | `cargo test -p rail-pimdir` | < 1 s |
| 10k latency budget (fails until phase 2) | `cargo test -p rail-pimdir --release --test budget -- --ignored --nocapture` | ~20 s |
| real neverest, shared store | `NEVEREST=/path/to/neverest scripts/neverest-shared-store.sh` (needs podman or docker) | ~1 min |
| pin vs upstream | `scripts/check-pimdir-pin.sh` | network |

Latest budget run (release, 10,000 messages across two accounts):

| Read | p50 | p95 |
|---|---|---|
| unified list, first page | 0.77 ms | 0.82 ms |
| unified list, page 101 | 0.78 ms | 0.82 ms |
| open one thread | 0.81 ms | 0.89 ms |
| threaded listing, first page | 455 ms | 475 ms |

Rail's inbox is a thread list, so the budget holds the threaded listing too, and it misses it:
the bench fails until phase 2 (constraint 6 in the plan).

[io-pimdir]: https://github.com/pimalaya/io-pimdir
