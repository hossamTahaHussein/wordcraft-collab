//! A document's fingerprint, for replicas to check they agree (spec §7).

use wordcraft_doc::Document;

/// How often (in revisions) replicas report their fingerprint.
pub const HASH_EVERY: u64 = 50;

/// FNV-1a (64-bit) of the document's native JSON, as 16 lowercase hex digits. A document that
/// can't be written gives the fingerprint of nothing.
pub fn doc_hash(doc: &Document) -> String {
    let bytes = wordcraft_engine::io::save_bytes("doc.json", doc).unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}
