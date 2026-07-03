//! Golden wire-contract proof (T5 spike — see notes/wire-single-source-spike.md).
//!
//! Contract: every Haskell-encoder-produced fixture must (1) decode, (2)
//! re-encode through the Rust writer to a byte-identical CBOR payload, and
//! (3) decode again to the same structure. The committed fixtures double as
//! the cross-language golden corpus: the Haskell side proves it still emits
//! these bytes, the Rust side proves it reads and reproduces them. Either
//! side drifting alone breaks its half of the contract.
//!
//! The committed corpus is legacy (headerless, 6-element meta entries), so it
//! also pins the reader's tolerance paths — a corpus regenerated from the
//! current encoder must be ADDED, not swapped in, or tolerance coverage is lost.

use std::collections::BTreeSet;
use std::path::PathBuf;

use tidepool_repr::frame::CoreFrame;
use tidepool_repr::serial::{read_cbor, read_metadata, write_cbor, write_metadata};
use tidepool_repr::types::{AltCon, Literal};

const CORPUS_DIRS: &[&str] = &["../haskell/test/Identity_cbor", "../haskell/test/TextSuite_cbor"];

/// Tree fixtures (everything except meta.cbor) and meta fixtures, by dir scan.
fn corpus() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut trees = Vec::new();
    let mut metas = Vec::new();
    for dir in CORPUS_DIRS {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("corpus dir {dir} missing: {e}"));
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|x| x == "cbor") {
                if path.file_name().is_some_and(|n| n == "meta.cbor") {
                    metas.push(path);
                } else {
                    trees.push(path);
                }
            }
        }
    }
    trees.sort();
    metas.sort();
    assert!(!trees.is_empty() && !metas.is_empty(), "empty golden corpus");
    (trees, metas)
}

/// CBOR payload without the optional 8-byte TPLR header.
fn payload(bytes: &[u8]) -> &[u8] {
    if bytes.len() >= 8 && bytes[..4] == *b"TPLR" {
        &bytes[8..]
    } else {
        bytes
    }
}

fn first_divergence(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Decode → re-encode → byte-compare payloads → decode again.
#[test]
fn tree_fixtures_roundtrip_byte_identically() {
    let (trees, _) = corpus();
    for path in &trees {
        let golden = std::fs::read(path).unwrap();
        let tree = read_cbor(&golden)
            .unwrap_or_else(|e| panic!("{}: decode failed: {e}", path.display()));
        let reencoded = write_cbor(&tree).unwrap();
        let (want, got) = (payload(&golden), payload(&reencoded));
        assert_eq!(
            want,
            got,
            "{}: re-encoded payload diverges at byte {} of {}",
            path.display(),
            first_divergence(want, got),
            want.len(),
        );
        let again = read_cbor(&reencoded).unwrap();
        assert_eq!(tree, again, "{}: second decode differs", path.display());
    }
    println!("byte-identical roundtrip: {} tree fixtures", trees.len());
}

/// Meta roundtrips SEMANTICALLY only: the two writers disagree today (Haskell
/// always emits 7-element entries + optional warnings keys; the Rust writer
/// emits 5/6/7 conditionally and drops warnings), so byte parity is asserted
/// for the table content, not the bytes. Closing that writer gap is a finding
/// of the spike, not this test's job.
#[test]
fn meta_fixtures_roundtrip_semantically() {
    let (_, metas) = corpus();
    for path in &metas {
        let golden = std::fs::read(path).unwrap();
        let (table, _warnings) = read_metadata(&golden)
            .unwrap_or_else(|e| panic!("{}: decode failed: {e}", path.display()));
        let reencoded = write_metadata(&table).unwrap();
        let (table2, _) = read_metadata(&reencoded).unwrap();
        assert_eq!(table, table2, "{}: table changed across roundtrip", path.display());
    }
    println!("semantic meta roundtrip: {} fixtures", metas.len());
}

/// A current-version TPLR header prepended to a legacy payload must decode to
/// the same tree — pins header stripping against the legacy pass-through.
#[test]
fn header_and_legacy_paths_agree() {
    let (trees, _) = corpus();
    let golden = std::fs::read(&trees[0]).unwrap();
    let legacy = read_cbor(&golden).unwrap();
    let mut headered = b"TPLR\x00\x01\x00\x01".to_vec();
    headered.extend_from_slice(payload(&golden));
    let via_header = read_cbor(&headered).unwrap();
    assert_eq!(legacy, via_header);
}

/// Corpus shape census. The matches are EXHAUSTIVE on purpose: adding a
/// CoreFrame variant, Literal variant, or AltCon variant fails this test's
/// compilation, forcing a corpus-coverage decision in the same commit as the
/// wire change. The floor below is what today's corpus actually covers; the
/// gap list is the corpus-generation TODO for the real implementation.
#[test]
fn corpus_shape_census() {
    let (trees, _) = corpus();
    let mut frames = BTreeSet::new();
    let mut lits = BTreeSet::new();
    let mut alts = BTreeSet::new();

    let lit_key = |l: &Literal, lits: &mut BTreeSet<&'static str>| {
        lits.insert(match l {
            Literal::LitInt(_) => "LitInt",
            Literal::LitWord(_) => "LitWord",
            Literal::LitChar(_) => "LitChar",
            Literal::LitString(_) => "LitString",
            Literal::LitByteArray(_) => "LitByteArray",
            Literal::LitFloat(_) => "LitFloat",
            Literal::LitDouble(_) => "LitDouble",
        });
    };

    for path in &trees {
        let tree = read_cbor(&std::fs::read(path).unwrap()).unwrap();
        for node in &tree.nodes {
            frames.insert(match node {
                CoreFrame::Var(_) => "Var",
                CoreFrame::Lit(l) => {
                    lit_key(l, &mut lits);
                    "Lit"
                }
                CoreFrame::App { .. } => "App",
                CoreFrame::Lam { .. } => "Lam",
                CoreFrame::LetNonRec { .. } => "LetNonRec",
                CoreFrame::LetRec { .. } => "LetRec",
                CoreFrame::Case { alts: cas, .. } => {
                    for alt in cas {
                        alts.insert(match &alt.con {
                            AltCon::DataAlt(_) => "DataAlt",
                            AltCon::LitAlt(l) => {
                                lit_key(l, &mut lits);
                                "LitAlt"
                            }
                            AltCon::Default => "Default",
                        });
                    }
                    "Case"
                }
                CoreFrame::Con { .. } => "Con",
                CoreFrame::Join { .. } => "Join",
                CoreFrame::Jump { .. } => "Jump",
                CoreFrame::PrimOp { .. } => "PrimOp",
            });
        }
    }

    println!("frames covered: {frames:?}");
    println!("literals covered: {lits:?}");
    println!("altcons covered: {alts:?}");

    let all_frames: BTreeSet<&str> = [
        "Var", "Lit", "App", "Lam", "LetNonRec", "LetRec", "Case", "Con", "Join", "Jump", "PrimOp",
    ]
    .into();
    let uncovered: Vec<_> = all_frames.difference(&frames).collect();
    println!("frame gaps to fill in a generated corpus: {uncovered:?}");

    // Floor: shapes any real Core corpus must exercise. Full coverage is the
    // generated corpus's acceptance criterion, not this committed one's.
    for required in ["Var", "Lit", "App", "Lam", "Case", "Con"] {
        assert!(frames.contains(required), "corpus lost {required} coverage");
    }
}
