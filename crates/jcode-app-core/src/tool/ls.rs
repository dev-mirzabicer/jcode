use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const DEFAULT_IGNORE: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "venv",
    ".venv",
    "coverage",
    ".cache",
];

pub struct LsTool;

impl LsTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct LsInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    ignore: Option<Vec<String>>,
}

struct Listing {
    output: String,
    capture: Option<std::sync::Arc<dyn jcode_tool_core::OutputCapture>>,
    stop: Option<jcode_agent_runtime::InterruptSignal>,
    files: usize,
    directories: usize,
}
impl Listing {
    fn emit(&mut self, text: &str) -> Result<()> {
        anyhow::ensure!(
            !self.stop.as_ref().is_some_and(|stop| stop.is_set()),
            "Directory listing cancelled"
        );
        self.output.push_str(text);
        if self.capture.is_some() && self.output.len() >= 64 * 1024 {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if let Some(capture) = &self.capture {
            capture.write(jcode_tool_core::OutputStream::Text, self.output.as_bytes())?;
            self.output.clear();
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for LsTool {
    fn execution_policy(
        &self,
        _: &Value,
        _: &ToolContext,
    ) -> Result<jcode_tool_core::ExecutionPolicy> {
        Ok(jcode_tool_core::ExecutionPolicy {
            cooperative_stop: true,
            ..Default::default()
        })
    }
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory contents."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "path": {
                    "type": "string",
                    "description": "Directory path."
                },
                "ignore": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ignore patterns."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: LsInput = serde_json::from_value(input)?;

        let base_path = params.path.clone().unwrap_or_else(|| ".".to_string());
        let base = ctx.resolve_path(Path::new(&base_path));
        let ignore_extra = params.ignore.clone();

        if !base.exists() {
            return Err(anyhow::anyhow!("Directory not found: {}", base_path));
        }

        if !base.is_dir() {
            return Err(anyhow::anyhow!("Not a directory: {}", base_path));
        }

        let capture = ctx.invocation.capture.clone();
        let stop = ctx.graceful_shutdown_signal.clone();
        tokio::task::spawn_blocking(move || {
            let mut ignore: Vec<String> =
                DEFAULT_IGNORE.iter().map(|name| name.to_string()).collect();
            if let Some(extra) = ignore_extra {
                ignore.extend(extra);
            }
            let mut listing = Listing {
                output: String::new(),
                capture,
                stop,
                files: 0,
                directories: 0,
            };
            listing.emit(&format!("{base_path}/\n"))?;
            let collected = collect_entries(&base, 0, &ignore, &mut listing);
            listing.flush()?;
            collected?;
            listing.emit(&format!(
                "\n{} files, {} directories",
                listing.files, listing.directories
            ))?;
            listing.flush()?;
            let mut output = ToolOutput::new(listing.output).with_metadata(
                json!({"files":listing.files,"directories":listing.directories,"max_depth":6}),
            );
            if let Some(capture) = listing.capture {
                output.source = jcode_tool_types::OutputSource::Retained(capture.reference()?);
            }
            Ok::<_, anyhow::Error>(output)
        })
        .await?
    }
}

fn collect_entries(
    dir: &Path,
    depth: usize,
    ignore: &[String],
    listing: &mut Listing,
) -> Result<()> {
    anyhow::ensure!(
        !listing.stop.as_ref().is_some_and(|stop| stop.is_set()),
        "Directory listing cancelled"
    );
    let mut items = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        anyhow::ensure!(
            !listing.stop.as_ref().is_some_and(|stop| stop.is_set()),
            "Directory listing cancelled"
        );
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        items.push((entry, is_dir));
    }
    items.sort_by(|(a, a_dir), (b, b_dir)| match (*a_dir, *b_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.file_name().cmp(&b.file_name()),
    });
    for (item, is_dir) in items {
        let name = item.file_name().to_string_lossy().to_string();
        if name.starts_with('.')
            || ignore.iter().any(|pattern| {
                glob::Pattern::new(pattern).is_ok_and(|pattern| pattern.matches(&name))
                    || name == *pattern
            })
        {
            continue;
        }
        listing.emit(&format!(
            "{}{}{}\n",
            "  ".repeat(depth + 1),
            name,
            if is_dir { "/" } else { "" }
        ))?;
        if is_dir {
            listing.directories += 1;
        } else {
            listing.files += 1;
        }
        if is_dir && depth < 5 {
            collect_entries(&item.path(), depth + 1, ignore, listing)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn listing_retains_every_selected_entry_and_preserves_ignore_policy() -> Result<()> {
        let directory = tempfile::tempdir()?;
        for index in 0..250 {
            std::fs::write(directory.path().join(format!("entry-{index:03}")), b"")?;
        }
        std::fs::create_dir(directory.path().join("node_modules"))?;
        std::fs::write(directory.path().join("node_modules/ignored"), b"")?;
        let ctx = ToolContext {
            session_id: "ls".into(),
            message_id: "message".into(),
            tool_call_id: "call".into(),
            working_dir: Some(directory.path().into()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let output = LsTool::new().execute(json!({"path":"."}), ctx).await?;
        assert!(output.output.contains("entry-249"));
        assert!(!output.output.contains("ignored"));
        assert_eq!(output.metadata.unwrap()["files"], 250);
        Ok(())
    }
}
