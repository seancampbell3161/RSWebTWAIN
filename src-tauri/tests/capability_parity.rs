//! The native and sidecar scan paths must negotiate the same capabilities.
//!
//! These are two separate crates with two hand-maintained copies of the same
//! TWAIN setup, and they have silently diverged before: `ICAP_COMPRESSION` and
//! `ICAP_PIXELFLAVOR` were added to the sidecar and not to the native path,
//! and the gap survived several reviews because nothing compared them.
//!
//! The capability calls themselves cannot be unit-tested — they dispatch into
//! `TWAINDSM.dll` — so this pins the next best thing: that both files agree on
//! *which* capabilities they set.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Every `ICAP_*` / `CAP_*` identifier a file references.
///
/// In these two files such identifiers appear only as arguments to
/// capability setters, so the set of mentions is the set of capabilities
/// configured.
fn capabilities_mentioned(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let is_start = bytes[i].is_ascii_uppercase()
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'));

        if is_start {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let word = &source[i..j];
            if word.starts_with("ICAP_") || word.starts_with("CAP_") {
                found.insert(word.to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }

    found
}

fn read(relative: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn both_scan_paths_negotiate_the_same_capabilities() {
    let native = capabilities_mentioned(&read("src/scanner/twain.rs"));
    let sidecar = capabilities_mentioned(&read("../scanner-sidecar/src/main.rs"));

    let only_native: Vec<_> = native.difference(&sidecar).collect();
    let only_sidecar: Vec<_> = sidecar.difference(&native).collect();

    assert!(
        only_native.is_empty() && only_sidecar.is_empty(),
        "the two scan paths configure different capabilities.\n\
         only in twain.rs:            {only_native:?}\n\
         only in scanner-sidecar:     {only_sidecar:?}\n\
         Add the missing ones to both, or this divergence ships."
    );
}

/// The capabilities whose absence caused real defects. Pinned by name so that
/// deleting one from both paths still fails rather than quietly passing the
/// parity check above.
#[test]
fn both_paths_pin_the_decoding_assumptions() {
    for (label, source) in [
        ("twain.rs", read("src/scanner/twain.rs")),
        ("scanner-sidecar", read("../scanner-sidecar/src/main.rs")),
    ] {
        let caps = capabilities_mentioned(&source);
        for required in [
            "ICAP_XFERMECH",
            "ICAP_COMPRESSION",
            "ICAP_PIXELFLAVOR",
            "ICAP_BITORDER",
        ] {
            assert!(
                caps.contains(required),
                "{label} does not negotiate {required}; the decode path assumes it"
            );
        }
    }
}
