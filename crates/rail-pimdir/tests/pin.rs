//! The io-pimdir pin holds together: what `Cargo.lock` locks, what the crate states, and the
//! draft the library writes. A store opens under one draft alone, so a drift here is a Rail
//! that cannot open the user's store.

use io_pimdir::client::reader::PimdirReader;
use rail_pimdir::fixture::Fixture;
use rail_pimdir::{IO_PIMDIR_REV, STORAGE_DRAFT};

#[test]
fn the_lockfile_locks_the_stated_rev() {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("the workspace Cargo.lock");
    let package = lock
        .split("[[package]]")
        .find(|package| package.contains("name = \"io-pimdir\""))
        .expect("Cargo.lock locks io-pimdir");
    let source = package
        .lines()
        .find_map(|line| line.strip_prefix("source = "))
        .expect("io-pimdir has a source");
    assert!(
        source.contains("github.com/pimalaya/io-pimdir")
            && source.ends_with(&format!("#{IO_PIMDIR_REV}\"")),
        "Cargo.lock locks io-pimdir from {source}, rail_pimdir::IO_PIMDIR_REV says {IO_PIMDIR_REV}",
    );
}

#[test]
fn the_stated_draft_is_the_librarys() {
    assert_eq!(STORAGE_DRAFT, io_pimdir::sql::FORMAT_VERSION);
}

#[test]
fn a_store_is_stamped_with_the_stated_draft() {
    // The draft a store is stamped with is what every later open checks (STORAGE §6), so this
    // is the value that must match the user's neverest.
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::create(dir.path()).unwrap();
    let stamped: String = rusqlite::Connection::open(fixture.dir().join("pimdir.db"))
        .unwrap()
        .query_row("SELECT format_version FROM store_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stamped, STORAGE_DRAFT);
    PimdirReader::open(fixture.dir()).expect("a reader opens a store of the pinned draft");
}
