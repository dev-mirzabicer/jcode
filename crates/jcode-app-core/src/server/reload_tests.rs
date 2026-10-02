use super::receive_reload_signal;
use crate::server::ReloadSignal;
use tokio::sync::watch;

#[tokio::test]
async fn receive_reload_signal_consumes_already_pending_value() {
    let (tx, mut rx) = watch::channel(None::<ReloadSignal>);
    tx.send(Some(ReloadSignal {
        hash: "abc1234".to_string(),
        triggering_session: Some("sess-1".to_string()),
        prefer_selfdev_binary: true,
        request_id: "reload-1".to_string(),
    }))
    .expect("send pending reload signal");

    let signal = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        receive_reload_signal(&mut rx, &mut None),
    )
    .await
    .expect("pending signal should be observed immediately")
    .expect("channel should still be open");

    assert_eq!(signal.hash, "abc1234");
    assert_eq!(signal.triggering_session.as_deref(), Some("sess-1"));
    assert!(signal.prefer_selfdev_binary);
    assert_eq!(signal.request_id, "reload-1");
}

#[tokio::test]
async fn receive_reload_signal_waits_for_future_value_when_initially_empty() {
    let (tx, mut rx) = watch::channel(None::<ReloadSignal>);

    let waiter = tokio::spawn(async move { receive_reload_signal(&mut rx, &mut None).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    tx.send(Some(ReloadSignal {
        hash: "def5678".to_string(),
        triggering_session: Some("sess-2".to_string()),
        prefer_selfdev_binary: false,
        request_id: "reload-2".to_string(),
    }))
    .expect("send future reload signal");

    let signal = tokio::time::timeout(std::time::Duration::from_millis(100), waiter)
        .await
        .expect("future signal should wake waiter")
        .expect("waiter task should succeed")
        .expect("channel should still be open");

    assert_eq!(signal.hash, "def5678");
    assert_eq!(signal.triggering_session.as_deref(), Some("sess-2"));
    assert!(!signal.prefer_selfdev_binary);
    assert_eq!(signal.request_id, "reload-2");
}
