//! Domain-separated mutual-authentication proofs for local IPC sessions.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use thiserror::Error;

use crate::{credentials::RootSecret, protocol::ClientHello};

const NONCE_BYTES: usize = 24;
const AUTH_DOMAIN: &[u8] = b"grimmore-ipc-auth-v1";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy)]
pub struct AuthTranscript<'a> {
    pub hello: &'a ClientHello,
    pub server_version: &'a str,
    pub session_id: &'a str,
    pub server_nonce: &'a str,
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("operating system random source error: {0}")]
    Random(#[from] getrandom::Error),
    #[error("authentication proof is not valid base64url")]
    InvalidEncoding,
    #[error("authentication proof did not match the session transcript")]
    InvalidProof,
}

pub fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[must_use]
pub fn server_proof(secret: &RootSecret, transcript: AuthTranscript<'_>) -> String {
    create_proof(secret, b"server", transcript)
}

#[must_use]
pub fn client_proof(secret: &RootSecret, transcript: AuthTranscript<'_>) -> String {
    create_proof(secret, b"client", transcript)
}

pub fn verify_server_proof(
    secret: &RootSecret,
    transcript: AuthTranscript<'_>,
    encoded_proof: &str,
) -> Result<(), AuthError> {
    verify_proof(secret, b"server", transcript, encoded_proof)
}

pub fn verify_client_proof(
    secret: &RootSecret,
    transcript: AuthTranscript<'_>,
    encoded_proof: &str,
) -> Result<(), AuthError> {
    verify_proof(secret, b"client", transcript, encoded_proof)
}

fn create_proof(secret: &RootSecret, actor: &[u8], transcript: AuthTranscript<'_>) -> String {
    let mac = transcript_mac(secret, actor, transcript);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn verify_proof(
    secret: &RootSecret,
    actor: &[u8],
    transcript: AuthTranscript<'_>,
    encoded_proof: &str,
) -> Result<(), AuthError> {
    let proof = URL_SAFE_NO_PAD
        .decode(encoded_proof)
        .map_err(|_| AuthError::InvalidEncoding)?;
    transcript_mac(secret, actor, transcript)
        .verify_slice(&proof)
        .map_err(|_| AuthError::InvalidProof)
}

fn transcript_mac(secret: &RootSecret, actor: &[u8], transcript: AuthTranscript<'_>) -> HmacSha256 {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(secret.expose())
        .expect("a 32-byte HMAC key is always valid");
    update_field(&mut mac, AUTH_DOMAIN);
    update_field(&mut mac, actor);
    update_field(&mut mac, &transcript.hello.protocol_version.to_be_bytes());
    update_field(&mut mac, transcript.hello.client_version.as_bytes());
    update_field(&mut mac, transcript.hello.role.as_str().as_bytes());
    update_field(&mut mac, transcript.hello.client_nonce.as_bytes());
    update_field(&mut mac, transcript.server_version.as_bytes());
    update_field(&mut mac, transcript.session_id.as_bytes());
    update_field(&mut mac, transcript.server_nonce.as_bytes());
    mac
}

fn update_field(mac: &mut HmacSha256, field: &[u8]) {
    let length = u64::try_from(field.len()).expect("authentication fields fit u64");
    mac.update(&length.to_be_bytes());
    mac.update(field);
}

#[cfg(test)]
mod tests {
    use super::{
        AuthTranscript, client_proof, server_proof, verify_client_proof, verify_server_proof,
    };
    use crate::{
        credentials::RootSecret,
        protocol::{ClientHello, PROTOCOL_VERSION, SessionRole},
    };

    fn hello() -> ClientHello {
        ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_version: "0.1.0".to_owned(),
            role: SessionRole::Plugin,
            client_nonce: "client-nonce".to_owned(),
        }
    }

    #[test]
    fn both_roles_prove_the_exact_session_transcript() {
        let secret = RootSecret::from_bytes([7; 32]);
        let hello = hello();
        let transcript = AuthTranscript {
            hello: &hello,
            server_version: "0.1.0",
            session_id: "session",
            server_nonce: "server-nonce",
        };
        let server = server_proof(&secret, transcript);
        let client = client_proof(&secret, transcript);

        verify_server_proof(&secret, transcript, &server).expect("verify server proof");
        verify_client_proof(&secret, transcript, &client).expect("verify client proof");
    }

    #[test]
    fn proof_replay_with_a_different_nonce_fails() {
        let secret = RootSecret::from_bytes([9; 32]);
        let hello = hello();
        let original = AuthTranscript {
            hello: &hello,
            server_version: "0.1.0",
            session_id: "session",
            server_nonce: "first",
        };
        let replayed = AuthTranscript {
            server_nonce: "second",
            ..original
        };
        let proof = client_proof(&secret, original);

        assert!(verify_client_proof(&secret, replayed, &proof).is_err());
    }
}
