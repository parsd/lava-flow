#[cfg(any(feature = "rustcrypto-auth", test))]
pub(super) const NONCE_SIZE: usize = 32;
#[cfg(any(feature = "rustcrypto-auth", test))]
pub(super) const TAG_SIZE: usize = 32;
#[cfg(feature = "rustcrypto-auth")]
pub(super) const TRANSCRIPT_DOMAIN: &[u8] = b"lava-flow local ipc auth v1";

#[cfg(feature = "rustcrypto-auth")]
mod rustcrypto;
#[cfg(not(feature = "rustcrypto-auth"))]
mod unsupported;

#[cfg(feature = "rustcrypto-auth")]
use rustcrypto as backend;
#[cfg(not(feature = "rustcrypto-auth"))]
use unsupported as backend;

#[cfg(not(feature = "rustcrypto-auth"))]
pub(super) use backend::unsupported_auth_error;
#[cfg(any(feature = "rustcrypto-auth", test))]
pub(super) use backend::{auth_tag, random_nonce, verify_auth_tag};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SharedSecret(Vec<u8>);

impl SharedSecret {
    pub(super) fn new(secret: Vec<u8>) -> std::result::Result<Self, AuthSetupError> {
        if secret.is_empty() {
            return Err(AuthSetupError::EmptySharedSecret);
        }
        backend::validate_available()?;
        Ok(Self(secret))
    }

    #[cfg(test)]
    pub(super) fn from_test_secret(secret: Vec<u8>) -> Self {
        Self(secret)
    }

    #[cfg(any(feature = "rustcrypto-auth", test))]
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedSecret").field(&"<redacted>").finish()
    }
}

#[cfg(feature = "rustcrypto-auth")]
impl Drop for SharedSecret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AuthSetupError {
    EmptySharedSecret,
    #[cfg(not(feature = "rustcrypto-auth"))]
    UnsupportedSharedSecretHmacSha256,
}

impl AuthSetupError {
    pub(super) fn to_error(&self) -> crate::error::LavaFlowError {
        match self {
            Self::EmptySharedSecret => crate::error::LavaFlowError::ChannelAuthenticationFailed {
                reason: "shared secret is empty",
            },
            #[cfg(not(feature = "rustcrypto-auth"))]
            Self::UnsupportedSharedSecretHmacSha256 => {
                crate::error::LavaFlowError::UnsupportedChannelAuthentication {
                    mechanism: "shared-secret-hmac-sha256",
                }
            }
        }
    }
}
