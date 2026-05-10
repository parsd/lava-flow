pub(super) fn validate_available() -> std::result::Result<(), super::AuthSetupError> {
    Err(super::AuthSetupError::UnsupportedSharedSecretHmacSha256)
}

pub(in crate::channel::local) fn unsupported_auth_error() -> crate::error::LavaFlowError {
    crate::error::LavaFlowError::UnsupportedChannelAuthentication {
        mechanism: "shared-secret-hmac-sha256",
    }
}

#[cfg(test)]
pub(in crate::channel::local) fn random_nonce() -> Result<[u8; super::NONCE_SIZE]> {
    Err(unsupported_auth_error())
}

#[cfg(test)]
pub(in crate::channel::local) fn auth_tag(
    secret: &super::SharedSecret,
    _transcript: &[u8],
) -> Result<[u8; super::TAG_SIZE]> {
    let _ = secret.as_bytes();
    Err(unsupported_auth_error())
}

#[cfg(test)]
pub(in crate::channel::local) fn verify_auth_tag(
    secret: &super::SharedSecret,
    _transcript: &[u8],
    _tag: &[u8],
) -> Result<()> {
    let _ = secret.as_bytes();
    Err(unsupported_auth_error())
}

#[cfg(test)]
use crate::error::Result;
