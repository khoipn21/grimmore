//! Length-prefixed JSON framing for private IPC.

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::MAX_FRAME_BYTES;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("IPC frame is empty")]
    Empty,
    #[error("IPC frame length {0} exceeds the configured limit")]
    TooLarge(usize),
    #[error("IPC transport error: {0}")]
    Io(#[from] std::io::Error),
    #[error("IPC frame is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn read_json<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(length));
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

pub async fn write_json<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let payload = serde_json::to_vec(message)?;
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(payload.len()));
    }

    let length = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(payload.len()))?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncWriteExt, duplex};

    use super::{FrameError, read_json, write_json};
    use crate::protocol::MAX_FRAME_BYTES;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Message {
        value: String,
    }

    #[tokio::test]
    async fn framed_json_round_trips_without_delimiters() {
        let (mut writer, mut reader) = duplex(128);
        let message = Message {
            value: "grimmore".to_owned(),
        };

        let (write_result, read_result) = tokio::join!(
            write_json(&mut writer, &message),
            read_json::<_, Message>(&mut reader)
        );
        write_result.expect("write bounded frame");
        assert_eq!(read_result.expect("read bounded frame"), message);
    }

    #[tokio::test]
    async fn oversized_header_is_rejected_before_allocation() {
        let (mut writer, mut reader) = duplex(16);
        let oversized = u32::try_from(MAX_FRAME_BYTES + 1).expect("frame cap fits u32");
        writer
            .write_all(&oversized.to_be_bytes())
            .await
            .expect("write test header");

        assert!(matches!(
            read_json::<_, Message>(&mut reader).await,
            Err(FrameError::TooLarge(_))
        ));
    }
}
