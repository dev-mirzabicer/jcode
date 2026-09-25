//! Real process exit is tested only in this owned, isolated child process.
use super::*;
use crate::protocol::{Request, ServerEvent};
use crate::workspace::{RequestId, runtime::*};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[test]
fn forced_server_subprocess_fixture() -> Result<()> {
    if std::env::var_os("JCODE_WP08_FORCE_CHILD").is_none() {
        return Ok(());
    }
    tokio::runtime::Runtime::new()?.block_on(async {
        let root = crate::storage::jcode_dir()?;
        let server = Arc::new(Server::new_with_paths(
            Arc::new(TestProvider),
            root.join("force.sock"),
            root.join("force-debug.sock"),
        ));
        let serving = server.clone();
        let task = tokio::spawn(async move { serving.run().await });
        tokio::time::timeout(Duration::from_secs(15), async {
            while server.runtime_lifecycle.get().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let gate = crate::runtime_lifecycle::admission::current_runtime()?.unwrap();
        let permit = gate.independent(
            RuntimeWorkKind::Preparation,
            "owned-blocking-fixture".into(),
            None,
        )?;
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let ready = root.join("force-ready");
        crate::runtime_lifecycle::admission::scope(Some(permit), async {
            crate::runtime_lifecycle::admission::spawn_blocking(move || {
                std::fs::write(ready, "blocking owner active").unwrap();
                let _ = wait.recv();
            });
        })
        .await;
        let result = task.await?;
        drop(release);
        result?;
        anyhow::bail!("Forced exit returned normally instead of terminating its process")
    })
}

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
fn explicit_force_exits_only_the_reviewed_fixture_and_retains_uncertainty() -> Result<()> {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    let stdout = std::fs::File::create(sandbox.root().join("force-child.log"))?;
    let mut child = OwnedChild(
        std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "server::startup_tests::forced_exit::forced_server_subprocess_fixture",
                "--nocapture",
            ])
            .env("JCODE_WP08_FORCE_CHILD", "1")
            .stdout(stdout.try_clone()?)
            .stderr(stdout)
            .spawn()?,
    );
    tokio::runtime::Runtime::new()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !sandbox.root().join("force-ready").exists() {
                anyhow::ensure!(child.0.try_wait()?.is_none(), "Fixture exited before readiness");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        let socket = sandbox.root().join("force.sock");
        let stream = crate::transport::Stream::connect(&socket).await?;
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        async fn exchange(read: &mut BufReader<crate::transport::ReadHalf>, write: &mut crate::transport::WriteHalf, id: u64, request: RuntimeRequest) -> Result<RuntimeResponse> {
            write.write_all((serde_json::to_string(&Request::RuntimeControl { id, request: Box::new(request) })? + "\n").as_bytes()).await?;
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(10), read.read_line(&mut line)).await??;
            match serde_json::from_str::<ServerEvent>(&line)? {
                ServerEvent::RuntimeResponse { id: reply, response } if reply == id => Ok(*response),
                other => anyhow::bail!("Wrong runtime reply: {other:?}"),
            }
        }
        let RuntimeResponse::Review(review) = exchange(&mut read, &mut write, 1, RuntimeRequest::Review { options: ShutdownOptions { strategy: StopStrategy::Interrupt, independent: IndependentTasks::Stop, quiescence_timeout_seconds: 1 }}).await? else { anyhow::bail!("Review missing"); };
        let RuntimeResponse::Operation(operation) = exchange(&mut read, &mut write, 2, RuntimeRequest::Begin { request: RequestId::new(), review: review.id }).await? else { anyhow::bail!("Operation missing"); };
        let blocked = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let result = exchange(&mut read, &mut write, 3, RuntimeRequest::Inspect { operation: operation.id }).await?;
                if let RuntimeResponse::Operation(current) = result && current.phase == ShutdownPhase::Blocked { return Ok::<_, anyhow::Error>(current); }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await??;
        assert!(child.0.try_wait()?.is_none(), "Ordinary timeout exited the runtime");
        write.write_all((serde_json::to_string(&Request::RuntimeControl { id: 4, request: Box::new(RuntimeRequest::Force { operation: blocked.id, expected_revision: blocked.revision }) })? + "\n").as_bytes()).await?;
        let status = tokio::time::timeout(Duration::from_secs(15), async {
            loop { if let Some(status) = child.0.try_wait()? { return Ok::<_, anyhow::Error>(status); } tokio::time::sleep(Duration::from_millis(20)).await; }
        }).await??;
        assert_eq!(status.code(), Some(2));
        let store = crate::runtime_lifecycle::RuntimeStopStore::new(&crate::storage::durable_state_dir(), &socket)?;
        let receipt = store.status()?.operation.unwrap();
        assert_eq!(receipt.id, operation.id);
        assert_eq!(receipt.phase, ShutdownPhase::Forced);
        assert!(!receipt.remaining.is_empty() && !receipt.issues.is_empty());
        assert!(!socket.exists());
        assert!(store.require_automatic_start().is_err());
        store.authorize_start()?;
        println!("WP08_FORCED_EXIT_CLEANUP {}", serde_json::json!({"child_pid":child.0.id(),"exit":2,"operation":operation.id,"parent_alive":std::process::id()}));
        Ok(())
    })
}
