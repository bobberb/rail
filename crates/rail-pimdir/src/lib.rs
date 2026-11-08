//! Rail's data layer over a pimdir store.
//!
//! neverest owns the store: it syncs every account into it and applies the actions queued
//! there. Rail links io-pimdir to read the store and to queue actions, and never writes it any
//! other way. See `docs/design/pimdir-port.md` for the whole plan.
//!
//! This is phase 0: the version pin and the fixture builder the later phases test against.

#[cfg(feature = "fixture")]
pub mod fixture;

/// The io-pimdir git rev Rail is built against, the workspace pin restated.
///
/// A store opens under exactly one spec draft (pimdir STORAGE §6) and is never migrated, so
/// Rail must read stores with the same io-pimdir as the neverest that writes them. neverest
/// and himalaya both pin this rev. `tests/pin.rs` fails when `Cargo.lock` locks another one.
pub const IO_PIMDIR_REV: &str = "5c18a026b9c85b470984192452c2aa66e060a3fe";

/// The storage draft [`IO_PIMDIR_REV`] stamps a store with and requires to open one: what Rail
/// names when it refuses a store another neverest wrote.
pub const STORAGE_DRAFT: &str = "storage-draft-09";
