use super::{SharedSecret, TAG_SIZE};
use crate::error::{LavaFlowError, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub(super) fn validate_available() -> std::result::Result<(), super::AuthSetupError> {
    Ok(())
}

pub(in crate::channel::local) fn random_nonce() -> Result<[u8; super::NONCE_SIZE]> {
    let mut nonce = [0_u8; super::NONCE_SIZE];
    getrandom::fill(&mut nonce).map_err(|source| LavaFlowError::ChannelTransportOperation {
        operation: "generate_auth_nonce",
        source: std::io::Error::other(source.to_string()),
    })?;
    Ok(nonce)
}

pub(in crate::channel::local) fn auth_tag(
    secret: &SharedSecret,
    transcript: &[u8],
) -> Result<[u8; TAG_SIZE]> {
    let mut mac = new_hmac(secret);
    mac.update(transcript);
    let bytes = mac.finalize().into_bytes();
    let mut tag = [0_u8; TAG_SIZE];
    tag.copy_from_slice(&bytes);
    Ok(tag)
}

pub(in crate::channel::local) fn verify_auth_tag(
    secret: &SharedSecret,
    transcript: &[u8],
    tag: &[u8],
) -> Result<()> {
    let mut mac = new_hmac(secret);
    mac.update(transcript);
    mac.verify_slice(tag)
        .map_err(|_| LavaFlowError::ChannelAuthenticationFailed {
            reason: "shared-secret authentication tag mismatch",
        })
}

fn new_hmac(secret: &SharedSecret) -> Hmac<Sha256> {
    Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any size")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret() -> SharedSecret {
        SharedSecret::from_test_secret(b"rustcrypto backend test secret".to_vec())
    }

    #[test]
    fn validate_available_succeeds() {
        validate_available().expect("rustcrypto backend is available when feature is enabled");
    }

    #[test]
    fn random_nonce_returns_expected_size() {
        let nonce = random_nonce().expect("generate nonce");
        assert_eq!(nonce.len(), super::super::NONCE_SIZE);
    }

    #[test]
    fn auth_tag_verifies_matching_transcript_and_rejects_mismatch() {
        let secret = test_secret();
        let tag = auth_tag(&secret, b"transcript").expect("create auth tag");

        verify_auth_tag(&secret, b"transcript", &tag).expect("matching tag verifies");
        let err = verify_auth_tag(&secret, b"different transcript", &tag)
            .expect_err("different transcript must fail verification");
        assert!(matches!(
            err,
            LavaFlowError::ChannelAuthenticationFailed { .. }
        ));
    }
}
