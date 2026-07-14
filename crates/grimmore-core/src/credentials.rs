//! Root-secret access through the operating system credential store.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use keyring::{Entry, Error as KeyringError};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const SERVICE: &str = "dev.grimmore.local";
const USERNAME: &str = "ipc-root-v1";
const ROOT_SECRET_BYTES: usize = 32;
const ROOT_SECRET_ENCODED_BYTES: usize = (ROOT_SECRET_BYTES / 3) * 4
    + match ROOT_SECRET_BYTES % 3 {
        0 => 0,
        1 => 2,
        _ => 3,
    };
const STORE_VALUE_PREFIX: &str = "grimmore-root-v1:";

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RootSecret([u8; ROOT_SECRET_BYTES]);

impl RootSecret {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; ROOT_SECRET_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn expose(&self) -> &[u8; ROOT_SECRET_BYTES] {
        &self.0
    }

    pub fn load() -> Result<Self, CredentialError> {
        let entry = Entry::new(SERVICE, USERNAME)?;
        let bytes = Zeroizing::new(entry.get_secret()?);
        Self::from_store_value(bytes.as_slice())
    }

    pub fn load_or_create() -> Result<Self, CredentialError> {
        let entry = Entry::new(SERVICE, USERNAME)?;
        match entry.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                Self::from_store_value(bytes.as_slice())
            }
            Err(KeyringError::NoEntry) => {
                let mut bytes = [0_u8; ROOT_SECRET_BYTES];
                getrandom::fill(&mut bytes)?;
                let secret = Self(bytes);
                secret.store_in(&entry)?;
                Ok(secret)
            }
            Err(error) => Err(CredentialError::Keyring(error)),
        }
    }

    /// Verifies that the operating-system credential store supports a complete
    /// isolated write/read/delete cycle using the same encoding as the root
    /// secret, without creating a product credential.
    pub fn verify_store() -> Result<(), CredentialError> {
        let mut suffix = [0_u8; 16];
        getrandom::fill(&mut suffix)?;
        let username = format!("doctor-probe-{}", hex::encode(suffix));
        let entry = Entry::new(SERVICE, &username)?;
        let mut probe = Zeroizing::new([0_u8; ROOT_SECRET_BYTES]);
        getrandom::fill(&mut *probe)?;
        let stored_probe = Self::encode_store_value(&probe);
        if let Err(error) = entry.set_secret(stored_probe.as_bytes()) {
            let _ = entry.delete_credential();
            return Err(CredentialError::Keyring(error));
        }

        let retrieved = entry.get_secret();
        let cleanup = entry.delete_credential();
        cleanup?;
        let retrieved = Zeroizing::new(retrieved?);
        let retrieved = Self::from_store_value(retrieved.as_slice())?;
        if retrieved.expose() != &*probe {
            return Err(CredentialError::ProbeMismatch);
        }
        match entry.get_secret() {
            Err(KeyringError::NoEntry) => Ok(()),
            Ok(_) => Err(CredentialError::ProbeNotDeleted),
            Err(error) => Err(CredentialError::Keyring(error)),
        }
    }

    fn try_from_slice(bytes: &[u8]) -> Result<Self, CredentialError> {
        let bytes: [u8; ROOT_SECRET_BYTES] = bytes
            .try_into()
            .map_err(|_| CredentialError::InvalidLength(bytes.len()))?;
        Ok(Self(bytes))
    }

    fn store_in(&self, entry: &Entry) -> Result<(), CredentialError> {
        let value = Self::encode_store_value(self.expose());
        entry.set_secret(value.as_bytes())?;
        Ok(())
    }

    fn encode_store_value(bytes: &[u8; ROOT_SECRET_BYTES]) -> Zeroizing<String> {
        let mut value = Zeroizing::new(String::with_capacity(
            STORE_VALUE_PREFIX.len() + ROOT_SECRET_ENCODED_BYTES,
        ));
        value.push_str(STORE_VALUE_PREFIX);
        // Encode directly into the zeroizing buffer to avoid a second secret-bearing String.
        URL_SAFE_NO_PAD.encode_string(bytes, &mut value);
        value
    }

    fn from_store_value(value: &[u8]) -> Result<Self, CredentialError> {
        if value.len() == ROOT_SECRET_BYTES {
            return Self::try_from_slice(value);
        }
        let encoded =
            std::str::from_utf8(value).map_err(|_| CredentialError::InvalidStoreEncoding)?;
        let encoded = encoded
            .strip_prefix(STORE_VALUE_PREFIX)
            .ok_or(CredentialError::InvalidStoreEncoding)?;
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| CredentialError::InvalidStoreEncoding)?,
        );
        Self::try_from_slice(decoded.as_slice())
    }
}

impl fmt::Debug for RootSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RootSecret([REDACTED])")
    }
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("operating system credential store error: {0}")]
    Keyring(#[from] KeyringError),
    #[error("operating system random source error: {0}")]
    Random(#[from] getrandom::Error),
    #[error("stored root secret has invalid length {0}")]
    InvalidLength(usize),
    #[error("stored root secret encoding is invalid")]
    InvalidStoreEncoding,
    #[error("credential-store probe returned unexpected data")]
    ProbeMismatch,
    #[error("credential-store probe credential remained after cleanup")]
    ProbeNotDeleted,
}

#[cfg(test)]
mod tests {
    use super::{CredentialError, ROOT_SECRET_BYTES, RootSecret, STORE_VALUE_PREFIX};

    #[test]
    fn store_encoding_preserves_binary_root_secrets_and_reads_legacy_values() {
        let bytes = [0xff_u8; ROOT_SECRET_BYTES];
        let stored = RootSecret::encode_store_value(&bytes);
        assert!(stored.starts_with(STORE_VALUE_PREFIX));
        assert!(stored.is_ascii());

        let decoded = RootSecret::from_store_value(stored.as_bytes())
            .expect("encoded root secret is readable");
        assert_eq!(decoded.expose(), &bytes);

        let legacy = RootSecret::from_store_value(&bytes).expect("legacy raw secret is readable");
        assert_eq!(legacy.expose(), &bytes);
    }

    #[test]
    fn store_encoding_rejects_invalid_values() {
        let error = RootSecret::from_store_value(b"grimmore-root-v1:not base64")
            .expect_err("malformed encoded root secret is rejected");
        assert!(matches!(error, CredentialError::InvalidStoreEncoding));
    }

    #[test]
    #[ignore = "requires a native operating-system credential store"]
    fn native_credential_store_round_trip_cleans_up() {
        RootSecret::verify_store().expect("credential-store probe succeeds and cleans up");
    }
}
