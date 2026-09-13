use super::*;
use std::time::{Duration, Instant};

#[test]
#[ignore = "entrypoint for the independent-process capture fixture"]
fn independent_session_writer_fixture() -> Result<()> {
    let root = std::env::var_os("JCODE_INSPECTION_WRITER_ROOT")
        .context("Missing owned writer fixture root")?;
    let root = std::path::PathBuf::from(root);
    let mut session = Session::create_with_id("process_target".into(), None, None);
    session.mark_active();
    for index in 0..40 {
        session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: format!("writer-{index}-α"),
                cache_control: None,
            }],
        );
        if index % 3 == 0 {
            session.title = Some(format!("checkpoint-{index}"));
        }
        session.save()?;
        if index % 4 == 0 || index == 39 {
            std::fs::write(root.join("writer-ready"), (index + 1).to_string())?;
            let start = Instant::now();
            loop {
                if std::fs::read_to_string(root.join("reader-ack"))
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                    .is_some_and(|count| count > index)
                {
                    break;
                }
                ensure!(
                    start.elapsed() < Duration::from_secs(10),
                    "Reader did not acknowledge the independent writer's checkpoint"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    Ok(())
}

#[test]
fn capture_is_coherent_across_an_independent_process_append_and_checkpoint_writer() -> Result<()> {
    struct Child(Option<std::process::Child>);
    impl Drop for Child {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("home"))?;
    std::fs::create_dir(root.path().join("runtime"))?;
    let mut child = Child(Some(
        std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "session::capture::tests::independent_session_writer_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("JCODE_INSPECTION_WRITER_ROOT", root.path())
            .env("JCODE_HOME", root.path())
            .env("HOME", root.path().join("home"))
            .env("JCODE_RUNTIME_DIR", root.path().join("runtime"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    ));
    let start = Instant::now();
    let mut observations = 0;
    let mut lengths = std::collections::BTreeSet::new();
    loop {
        ensure!(
            start.elapsed() < Duration::from_secs(30),
            "Independent capture fixture exceeded its total deadline"
        );
        let ready = std::fs::read_to_string(root.path().join("writer-ready"))
            .ok()
            .and_then(|text| text.parse::<usize>().ok());
        if let Some(ready) = ready {
            let captured = Session::capture_readonly(root.path(), "process_target")?;
            let messages = &captured.session().messages;
            let identities: std::collections::HashSet<_> =
                messages.iter().map(|message| &message.id).collect();
            assert_eq!(identities.len(), messages.len());
            for (index, message) in messages.iter().enumerate() {
                assert!(
                    matches!(message.content.as_slice(),[crate::message::ContentBlock::Text{text,..}] if text==&format!("writer-{index}-α"))
                );
            }
            assert!(messages.len() >= ready && messages.len() <= 40);
            lengths.insert(messages.len());
            observations += 1;
            std::fs::write(root.path().join("reader-ack"), messages.len().to_string())?;
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
        if child.0.as_mut().unwrap().try_wait()?.is_some() {
            break;
        }
    }
    let output = child.0.take().unwrap().wait_with_output()?;
    ensure!(
        output.status.success(),
        "Independent writer failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        observations >= 10 && lengths.len() >= 10,
        "Fixture did not exercise multiple committed states"
    );
    assert_eq!(
        Session::capture_readonly(root.path(), "process_target")?
            .session()
            .messages
            .len(),
        40
    );
    Ok(())
}
