//! Lifetime of a request producer whose only consumer is an event receiver.
//! Closing that receiver cancels the owned future even during a quiet network
//! read or retry backoff. It never terminates a shared provider process/service.
use anyhow::Result;
use jcode_message_types::StreamEvent;
use std::future::Future;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub fn spawn_request(
    consumer: mpsc::Sender<Result<StreamEvent>>,
    request: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = request => {},
            _ = consumer.closed() => {},
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn quiet_request_and_backoff_stop_when_consumer_disconnects() {
        let (sender, receiver) = mpsc::channel(8);
        let dropped = Arc::new(AtomicBool::new(false));
        let owned = Dropped(dropped.clone());
        let task = spawn_request(sender, async move {
            let _owned = owned;
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn completion_keeps_buffered_output_and_closes_after_producer_finishes() {
        let (sender, mut receiver) = mpsc::channel(8);
        let task = spawn_request(sender.clone(), async move {
            sender
                .send(Ok(StreamEvent::TextDelta("complete".into())))
                .await
                .unwrap();
        });
        task.await.unwrap();
        assert!(
            matches!(receiver.recv().await.unwrap().unwrap(), StreamEvent::TextDelta(text) if text == "complete")
        );
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn dropping_http_consumer_closes_the_owned_quiet_transport() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let (headers_sent, headers_received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).await?;
                request.push(byte[0]);
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await?;
            let _ = headers_sent.send(());
            let mut byte = [0];
            let closed =
                tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte)).await?;
            match closed {
                Ok(0) => {}
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                other => anyhow::bail!("Owned quiet HTTP response did not close: {other:?}"),
            }
            Ok::<_, anyhow::Error>(())
        });
        let (sender, receiver) = mpsc::channel(8);
        let task = spawn_request(sender, async move {
            let response = reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(url)
                .send()
                .await
                .unwrap();
            let _ = response.bytes().await;
        });
        headers_received.await?;
        drop(receiver);
        task.await?;
        server.await??;
        Ok(())
    }
}
