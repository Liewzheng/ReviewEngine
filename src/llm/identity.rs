//! Provider card identity: the four-tuple entry fingerprint (RENG-75).
//!
//! Provider names are display labels, not identities — two cards may share a
//! `provider` name (two accounts of the same service, one account with two
//! models). What uniquely identifies a configured card is the tuple
//! `(provider, api_base, model, api_key)`, hashed so the identity can be
//! recorded alongside every usage/latency row without ever storing a second
//! copy of the secret next to it:
//!
//! - **Same tuple → same fingerprint.** Usage and latency aggregates fold by
//!   it, so two same-named cards each report only their own numbers. The
//!   connection-health cache keys on it too
//!   ([`crate::server::api::llm_health`]) — one definition of "which card is
//!   this" for the whole server.
//! - **Any field changes → a new fingerprint.** Rotating a key or moving the
//!   endpoint starts the card's statistics over; the old fingerprint's rows
//!   stay in the database but are no longer attributed to the card.
//! - **An empty `api_key` still fingerprints** (local/keyless providers):
//!   provider + URL + model alone distinguish those cards.
//!
//! # Security contract
//!
//! The fingerprint is `sha256` of a string that CONTAINS THE API KEY. A
//! truncated hash of a weak key is still offline-bruteforceable, so the
//! fingerprint must NEVER appear in an API response, a log line, or the UI.
//! The server does the "this config ↔ that statistics bucket" matching
//! internally ([`crate::models::LLMConfig::entry_fp`]) and responses carry
//! only the aggregated values. At rest it lives only inside
//! `llm_call_samples.entry_fp` and the `reviews.llm_summary` JSON, next to
//! the (encrypted/plaintext-by-design) rows they describe.

use sha2::{Digest, Sha256};

/// Length of the fingerprint in hex characters: the first 12 of the SHA-256
/// digest (48 bits). Fixed here so every reader/writer agrees — long enough
/// that two distinct cards collide only by astronomical accident, short
/// enough to stay a label in the rows that carry it.
pub const ENTRY_FP_HEX_LEN: usize = 12;

/// The fingerprint of one configured provider entry: the first
/// [`ENTRY_FP_HEX_LEN`] hex chars of
/// `sha256("{provider}\n{api_base}\n{model}\n{api_key}")`.
///
/// The `\n` separators make the encoding unambiguous (no pair of distinct
/// tuples can concatenate to the same string), and the field order —
/// provider, URL, model, key — is part of the contract: every write path
/// (call samples, `llm_summary`) and every read path (the two aggregates)
/// uses this one function.
pub fn entry_fp(provider: &str, api_base: &str, model: &str, api_key: &str) -> String {
    let mut hasher = Sha256::new();
    for field in [provider, api_base, model, api_key] {
        hasher.update(field.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hex::encode(hasher.finalize());
    digest[..ENTRY_FP_HEX_LEN].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fingerprint is deterministic and has the documented shape.
    #[test]
    fn fingerprint_is_deterministic_and_truncated() {
        let a = entry_fp("openai", "https://api.openai.com/v1", "gpt-4o", "sk-a");
        assert_eq!(a, entry_fp("openai", "https://api.openai.com/v1", "gpt-4o", "sk-a"));
        assert_eq!(a.len(), ENTRY_FP_HEX_LEN);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "hex only: {a}");
    }

    /// Every field of the tuple is part of the identity: change any one and
    /// the fingerprint changes — including the key (two accounts of one
    /// provider with the same model are two cards).
    #[test]
    fn every_tuple_field_changes_the_fingerprint() {
        let base = entry_fp("openai", "https://api.openai.com/v1", "gpt-4o", "sk-a");
        for changed in [
            entry_fp("azure", "https://api.openai.com/v1", "gpt-4o", "sk-a"),
            entry_fp("openai", "https://other.example/v1", "gpt-4o", "sk-a"),
            entry_fp("openai", "https://api.openai.com/v1", "gpt-4o-mini", "sk-a"),
            entry_fp("openai", "https://api.openai.com/v1", "gpt-4o", "sk-b"),
        ] {
            assert_ne!(base, changed, "a one-field change must re-fingerprint");
        }
        // An empty key still fingerprints (keyless/local providers).
        assert_ne!(
            base,
            entry_fp("openai", "https://api.openai.com/v1", "gpt-4o", ""),
            "empty key is a different entry, not an error"
        );
    }

    /// The output must never carry a fragment of the secret it hashes.
    #[test]
    fn fingerprint_leaks_no_key_material() {
        let key = "sk-super-secret-key-12345";
        let fp = entry_fp("openai", "https://api.openai.com/v1", "gpt-4o", key);
        for fragment in ["sk-super", "secret", "12345"] {
            assert!(!fp.contains(fragment), "fingerprint must not contain key fragments");
        }
        assert!(!fp.contains(key));
    }

    /// Field boundaries are unambiguous: no two different tuples can
    /// concatenate to the same preimage.
    #[test]
    fn field_separators_prevent_concatenation_ambiguity() {
        assert_ne!(
            entry_fp("ab", "c", "m", "k"),
            entry_fp("a", "bc", "m", "k"),
            "`\\n` separators keep (ab,c) and (a,bc) apart"
        );
    }
}
