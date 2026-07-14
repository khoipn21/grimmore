//! Timed mutual-authentication handshake over an already verified local endpoint.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

use crate::{
    auth::{
        AuthError, AuthTranscript, client_proof, random_token, server_proof, verify_client_proof,
        verify_server_proof,
    },
    credentials::RootSecret,
    framing::{FrameError, read_json, write_json},
    protocol::{
        ClientAuthenticate, ClientHandshakeMessage, ClientHello, PROTOCOL_VERSION, ServerChallenge,
        ServerHandshakeMessage, SessionReady, SessionRole,
    },
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSession {
    pub ready: SessionReady,
    pub peer_version: String,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("IPC handshake timed out")]
    Timeout,
    #[error("IPC framing failed: {0}")]
    Frame(#[from] FrameError),
    #[error("IPC authentication failed: {0}")]
    Auth(#[from] AuthError),
    #[error("peer uses unsupported protocol version {0}")]
    UnsupportedProtocol(u16),
    #[error("peer rejected the session: {0}")]
    Rejected(String),
    #[error("peer sent an unexpected handshake message")]
    UnexpectedMessage,
    #[error("system clock cannot represent the session expiry")]
    InvalidClock,
}

pub async fn authenticate_client<S>(
    stream: &mut S,
    secret: &RootSecret,
    role: SessionRole,
    client_version: &str,
) -> Result<AuthenticatedSession, SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(HANDSHAKE_TIMEOUT, async {
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_version: client_version.to_owned(),
            role,
            client_nonce: random_token()?,
        };
        write_json(stream, &ClientHandshakeMessage::Hello(hello.clone())).await?;
        let challenge = match read_json::<_, ServerHandshakeMessage>(stream).await? {
            ServerHandshakeMessage::Challenge(challenge) => challenge,
            ServerHandshakeMessage::Rejected { message } => {
                return Err(SessionError::Rejected(message));
            }
            ServerHandshakeMessage::Ready(_) => return Err(SessionError::UnexpectedMessage),
        };
        if challenge.protocol_version != PROTOCOL_VERSION {
            return Err(SessionError::UnsupportedProtocol(
                challenge.protocol_version,
            ));
        }
        let transcript = AuthTranscript {
            hello: &hello,
            server_version: &challenge.server_version,
            session_id: &challenge.session_id,
            server_nonce: &challenge.server_nonce,
        };
        verify_server_proof(secret, transcript, &challenge.server_proof)?;
        write_json(
            stream,
            &ClientHandshakeMessage::Authenticate(ClientAuthenticate {
                session_id: challenge.session_id.clone(),
                client_proof: client_proof(secret, transcript),
            }),
        )
        .await?;

        let ready = match read_json::<_, ServerHandshakeMessage>(stream).await? {
            ServerHandshakeMessage::Ready(ready) => ready,
            ServerHandshakeMessage::Rejected { message } => {
                return Err(SessionError::Rejected(message));
            }
            ServerHandshakeMessage::Challenge(_) => {
                return Err(SessionError::UnexpectedMessage);
            }
        };
        if ready.session_id != challenge.session_id || ready.role != role {
            return Err(SessionError::UnexpectedMessage);
        }
        Ok(AuthenticatedSession {
            ready,
            peer_version: challenge.server_version,
        })
    })
    .await
    .map_err(|_| SessionError::Timeout)?
}

pub async fn authenticate_server<S>(
    stream: &mut S,
    secret: &RootSecret,
    server_version: &str,
) -> Result<AuthenticatedSession, SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(HANDSHAKE_TIMEOUT, async {
        let ClientHandshakeMessage::Hello(hello) =
            read_json::<_, ClientHandshakeMessage>(stream).await?
        else {
            return Err(SessionError::UnexpectedMessage);
        };
        if hello.protocol_version != PROTOCOL_VERSION {
            let _ = write_json(
                stream,
                &ServerHandshakeMessage::Rejected {
                    message: "incompatible protocol version".to_owned(),
                },
            )
            .await;
            return Err(SessionError::UnsupportedProtocol(hello.protocol_version));
        }

        let session_id = random_token()?;
        let server_nonce = random_token()?;
        let transcript = AuthTranscript {
            hello: &hello,
            server_version,
            session_id: &session_id,
            server_nonce: &server_nonce,
        };
        let proof = server_proof(secret, transcript);
        write_json(
            stream,
            &ServerHandshakeMessage::Challenge(ServerChallenge {
                protocol_version: PROTOCOL_VERSION,
                server_version: server_version.to_owned(),
                session_id: session_id.clone(),
                server_nonce: server_nonce.clone(),
                server_proof: proof,
            }),
        )
        .await?;

        let ClientHandshakeMessage::Authenticate(authenticate) =
            read_json::<_, ClientHandshakeMessage>(stream).await?
        else {
            return Err(SessionError::UnexpectedMessage);
        };
        if authenticate.session_id != session_id
            || verify_client_proof(secret, transcript, &authenticate.client_proof).is_err()
        {
            let _ = write_json(
                stream,
                &ServerHandshakeMessage::Rejected {
                    message: "authentication rejected".to_owned(),
                },
            )
            .await;
            return Err(SessionError::Auth(AuthError::InvalidProof));
        }

        let ready = SessionReady {
            session_id,
            role: hello.role,
            expires_at_unix_ms: session_expiry()?,
        };
        write_json(stream, &ServerHandshakeMessage::Ready(ready.clone())).await?;
        Ok(AuthenticatedSession {
            ready,
            peer_version: hello.client_version,
        })
    })
    .await
    .map_err(|_| SessionError::Timeout)?
}

fn session_expiry() -> Result<u64, SessionError> {
    let expiry = SystemTime::now()
        .checked_add(SESSION_TTL)
        .ok_or(SessionError::InvalidClock)?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SessionError::InvalidClock)?;
    u64::try_from(expiry.as_millis()).map_err(|_| SessionError::InvalidClock)
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::{authenticate_client, authenticate_server};
    use crate::{credentials::RootSecret, protocol::SessionRole};

    #[tokio::test]
    async fn client_and_server_establish_the_same_role_scoped_session() {
        let secret = RootSecret::from_bytes([11; 32]);
        let (mut client_stream, mut server_stream) = duplex(4096);

        let (client, server) = tokio::join!(
            authenticate_client(
                &mut client_stream,
                &secret,
                SessionRole::McpReadonly,
                "client-0.1.0"
            ),
            authenticate_server(&mut server_stream, &secret, "server-0.1.0")
        );
        let client = client.expect("client authenticates server");
        let server = server.expect("server authenticates client");

        assert_eq!(client.ready.session_id, server.ready.session_id);
        assert_eq!(client.ready.role, SessionRole::McpReadonly);
        assert_eq!(client.peer_version, "server-0.1.0");
        assert_eq!(server.peer_version, "client-0.1.0");
    }
}
