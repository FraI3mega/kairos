//! Proof Key for Code Exchange (RFC 7636, S256): the program that starts a sign-in keeps a random verifier and hands
//! out only its hash, so a code caught on the way back is useless to anyone else.

use base64::Engine;
use sha2::{Digest, Sha256};

pub fn verifier() -> anyhow::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("os random: {e}"))?;
    Ok(b64url(&b))
}

pub fn challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

pub fn is_verifier(s: &str) -> bool {
    (43..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

pub fn is_challenge(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_example() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn verifiers_and_challenges_have_their_shape() {
        let v = verifier().unwrap();
        assert!(is_verifier(&v), "{v}");
        assert!(is_challenge(&challenge(&v)));
        assert_ne!(v, verifier().unwrap());
        assert!(!is_verifier("short"));
        assert!(!is_verifier(&"a".repeat(129)));
        assert!(!is_verifier(&format!("{}+", "a".repeat(42))));
        assert!(!is_challenge(&"a".repeat(42)));
        assert!(!is_challenge(&format!("{}=", "a".repeat(42))));
    }
}
