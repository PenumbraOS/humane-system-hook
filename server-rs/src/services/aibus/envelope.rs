use tonic::Status;

use crate::proto::common::encryption::{self, EncryptedData};

impl EncryptedData {
    /// Create an unencrypted EncryptedData envelope with the given kid and plaintext proto bytes.
    /// `kid` must be the fully-qualified Java class name of the inner proto.
    pub fn new(kid: &str, data: Vec<u8>) -> Self {
        encryption::EncryptedData {
            encryption_information: Some(encryption::EncryptionInformation { kid: kid.into() }),
            data,
        }
    }

    /// Create a stub EncryptedData envelope with the given kid and empty data.
    pub fn stub(kid: &str) -> Self {
        Self::new(kid, Vec::new())
    }
}

/// Decode the plaintext proto bytes from an EncryptedData envelope.
pub(super) fn unwrap_plaintext_data(
    encrypted: &Option<encryption::EncryptedData>,
) -> Result<&[u8], Status> {
    encrypted
        .as_ref()
        .map(|ed| ed.data.as_slice())
        .ok_or_else(|| Status::invalid_argument("missing encrypted data envelope"))
}
