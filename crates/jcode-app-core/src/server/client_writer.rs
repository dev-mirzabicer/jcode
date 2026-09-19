use crate::protocol::{ServerEvent, encode_event};
use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

pub(super) async fn write_direct_event(
    writer: &Arc<Mutex<crate::transport::WriteHalf>>,
    event: &ServerEvent,
) -> Result<()> {
    let json = encode_event(event);
    let mut w = writer.lock().await;
    write_bytes(&mut *w, json.as_bytes()).await?;
    Ok(())
}

pub(super) async fn write_bytes<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> std::io::Result<()> {
    write_with_progress_deadline(writer, bytes, std::time::Duration::from_secs(30)).await
}

async fn write_with_progress_deadline<W: AsyncWrite + Unpin>(
    writer: &mut W,
    mut bytes: &[u8],
    deadline: std::time::Duration,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written =
            tokio::time::timeout(deadline, writer.write(&bytes[..bytes.len().min(65536)]))
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Client socket made no write progress",
                    )
                })??;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn stopped_reader_times_out_with_only_the_written_prefix() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        let error = write_with_progress_deadline(
            &mut writer,
            &[9; 16],
            std::time::Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        drop(writer);
        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, vec![9; 8]);
    }

    #[tokio::test]
    async fn complete_large_response_is_delivered_without_truncation() {
        let expected = vec![7; 1024 * 1024];
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let (written, received) = tokio::join!(
            async {
                let result = write_bytes(&mut writer, &expected).await;
                drop(writer);
                result
            },
            async {
                let mut received = Vec::new();
                reader.read_to_end(&mut received).await.unwrap();
                received
            }
        );
        written.unwrap();
        assert_eq!(received, expected);
    }
}
