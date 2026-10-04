//! Key material: importing, generating, and the NIP-49 wrapping the vault
//! stores.
//!
//! Every decoder here delegates to the `nostr` crate — the point of this
//! module is not to know the formats but to narrow them to the surfaces
//! the layer actually offers, so a caller cannot accidentally accept
//! something the import UI never promised. The import surfaces, per the
//! plan: nsec (bech32), hex, NIP-06 mnemonic, or NIP-49 `ncryptsec` with
//! the passphrase it was wrapped in. Generating is the nostr crate's own
//! `SecretKey::generate`, called directly.

use anyhow::{bail, Result};
use nostr::key::{Keys, SecretKey};
use nostr::nips::nip06::FromMnemonic;
use nostr::nips::nip19::FromBech32;
use nostr::nips::nip49::{EncryptedSecretKey, KeySecurity};

/// The scrypt strength NIP-49 wraps at: 2^18, Opal's measured default.
/// Higher costs seconds per unlock on the machines this runs on; lower
/// spends the format's protection faster than it needs to.
pub const WRAP_LOG_N: u8 = 18;

/// The three-word-per-12/24-word split a mnemonic import accepts. NIP-06
/// derives from BIP-39 English; anything the wordlist rejects is not a
/// mnemonic this layer was offered.
pub fn from_mnemonic(phrase: &str) -> Result<SecretKey> {
    let words = phrase.split_whitespace().count();
    if words != 12 && words != 24 {
        bail!("a mnemonic is 12 or 24 words, got {words}");
    }
    let keys =
        Keys::from_mnemonic(phrase, None).map_err(|e| anyhow::anyhow!("mnemonic rejected: {e}"))?;
    Ok(keys.secret_key().clone())
}

/// Import a secret key from anything the layer promised to accept that is
/// not an `ncryptsec`: nsec bech32 or bare hex, or a NIP-06 mnemonic.
/// The dispatch is try-in-order and the formats cannot collide — a
/// mnemonic is words, an nsec is `nsec1…`, hex is `[0-9a-f]{64}` — so
/// order is presentation, not semantics.
pub fn import(raw: &str) -> Result<SecretKey> {
    let raw = raw.trim();
    if let Ok(key) = SecretKey::parse(raw) {
        return Ok(key);
    }
    if raw.split_whitespace().count() > 1 {
        return from_mnemonic(raw);
    }
    bail!("not an nsec, not hex, and not a mnemonic");
}

/// Decode an `ncryptsec` string with the passphrase it was wrapped in.
/// This is the one import surface that carries its own secret twice: the
/// wrapped key and the password that opens it.
pub fn decrypt_ncryptsec(ncryptsec: &str, passphrase: &str) -> Result<SecretKey> {
    let encrypted = EncryptedSecretKey::from_bech32(ncryptsec.trim())
        .map_err(|e| anyhow::anyhow!("not an ncryptsec: {e}"))?;
    encrypted
        .decrypt(passphrase)
        .map_err(|e| anyhow::anyhow!("ncryptsec did not decrypt with that passphrase: {e}"))
}

/// The key's public half, hex — the form the vault blob stores in the
/// clear and the form NIP-46's answers carry.
pub fn public_key_hex(key: &SecretKey) -> String {
    Keys::new(key.clone()).public_key().to_string()
}

/// Wrap a secret key as NIP-49 `ncryptsec` at the layer's strength, for
/// storage. `Medium` is the honest `KeySecurity` here: from this call on,
/// the key is only ever handled in its wrapped form.
pub fn to_ncryptsec(key: &SecretKey, passphrase: &str) -> Result<EncryptedSecretKey> {
    EncryptedSecretKey::new(key, passphrase, WRAP_LOG_N, KeySecurity::Medium)
        .map_err(|e| anyhow::anyhow!("wrapping the key failed: {e}"))
}

/// The bar a newly chosen passphrase must clear. This guards the
/// independent-passphrase vault mode (the gate-style default generates
/// its own wrap and never asks); the check exists now so the future mode
/// inherits a tested gate rather than inventing one under time pressure.
///
/// Deliberately not a strength estimator — no entropy arithmetic, no
/// wordlist dependency. Three refusals a person can act on: too short,
/// one repeated character, or too uniform in class (all letters, all
/// digits). The length bar is 16, not 8: the thing protected is a
/// nostr identity, whose compromise is silent and total.
pub fn passphrase_strength(passphrase: &str) -> Result<()> {
    if passphrase.chars().count() < 16 {
        bail!("passphrase is shorter than 16 characters");
    }
    let mut chars = passphrase.chars().collect::<Vec<_>>();
    chars.sort_unstable();
    chars.dedup();
    if chars.len() == 1 {
        bail!("passphrase is one character repeated");
    }
    let any_digit = passphrase.chars().any(|c| c.is_ascii_digit());
    let any_letter = passphrase.chars().any(|c| c.is_alphabetic());
    if !any_digit || !any_letter {
        bail!("passphrase must mix letters and digits");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // ToBech32 is a trait (nip19) and every call site is here.
    use nostr::nips::nip19::ToBech32;

    /// A mnemonic that decodes. Not a vector anyone's funds depend on —
    /// the assertion is that the same phrase derives the same key twice
    /// and a different phrase does not, which is what import depends on.
    const MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn import_accepts_hex_nsec_and_mnemonic() {
        let key = SecretKey::generate();
        let hex = key.to_secret_hex();
        assert_eq!(import(&hex).unwrap(), key);
        let nsec = key.to_bech32().unwrap();
        assert_eq!(import(&nsec).unwrap(), key);
        let from_words = import(MNEMONIC).unwrap();
        assert_eq!(import(MNEMONIC).unwrap(), from_words);
        assert_ne!(from_words, key, "a fixed phrase cannot derive a random key");
    }

    #[test]
    fn import_rejects_the_ungodly() {
        assert!(import("not a key at all").is_err());
        assert!(import("nsec1notarealkey").is_err());
        // An 11-word phrase is a truncated mnemonic, not a partial parse.
        let truncated = MNEMONIC.split_whitespace().take(11).collect::<Vec<_>>().join(" ");
        assert!(import(&truncated).is_err());
    }

    #[test]
    fn ncryptsec_round_trips_at_the_recorded_strength() {
        let key = SecretKey::generate();
        let wrapped = to_ncryptsec(&key, "a passphrase of substance").unwrap();
        assert_eq!(wrapped.log_n(), WRAP_LOG_N);
        assert_eq!(wrapped.decrypt("a passphrase of substance").unwrap(), key);
        assert!(wrapped.decrypt("the wrong one").is_err());
        // The bech32 form is what storage holds, so the string round-trips
        // through the same parse the import surface uses.
        let bech32 = wrapped.to_bech32().unwrap();
        assert_eq!(decrypt_ncryptsec(&bech32, "a passphrase of substance").unwrap(), key);
        assert!(decrypt_ncryptsec(&bech32, "wrong").is_err());
        assert!(decrypt_ncryptsec("ncryptsec1nonsense", "x").is_err());
    }

    #[test]
    fn passphrase_strength_draws_the_line_where_the_docs_say() {
        assert!(passphrase_strength("short").is_err());
        assert!(passphrase_strength("aaaaaaaaaaaaaaaa").is_err());
        assert!(passphrase_strength("all letters no digits").is_err());
        assert!(passphrase_strength("1234567890123456").is_err());
        assert!(passphrase_strength("a licence to decode 4412").is_ok());
    }
}
