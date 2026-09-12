use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::gmail::{self, GmailClient, MessageFormat};

pub struct GmailTool {
    client: GmailClient,
}

impl GmailTool {
    pub fn new() -> Self {
        Self {
            client: GmailClient::new(),
        }
    }
}

#[derive(Deserialize)]
struct GmailInput {
    action: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    draft_id: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    in_reply_to: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
    #[serde(default)]
    label_ids: Option<Vec<String>>,
    #[serde(default)]
    add_labels: Option<Vec<String>>,
    #[serde(default)]
    remove_labels: Option<Vec<String>>,
    #[serde(default)]
    confirmed: Option<bool>,
    #[serde(default)]
    attachments: Option<Vec<String>>,
}

#[async_trait]
impl Tool for GmailTool {
    fn name(&self) -> &str {
        "gmail"
    }

    fn description(&self) -> &str {
        "Use Gmail."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["connect", "search", "read", "list", "draft", "send", "send_draft", "threads", "thread", "labels", "trash", "modify_labels"],
                    "description": "Action. 'connect' sets up Gmail access via a browser OAuth screen the user approves."
                },
                "query": { "type": "string" },
                "message_id": { "type": "string" },
                "thread_id": { "type": "string" },
                "draft_id": { "type": "string" },
                "to": { "type": "string" },
                "subject": { "type": "string" },
                "body": { "type": "string" },
                "in_reply_to": { "type": "string" },
                "max_results": { "type": "integer" },
                "label_ids": { "type": "array", "items": { "type": "string" } },
                "add_labels": { "type": "array", "items": { "type": "string" } },
                "remove_labels": { "type": "array", "items": { "type": "string" } },
                "confirmed": {
                    "type": "boolean",
                    "description": "Confirm."
                },
                "attachments": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Absolute file paths to attach (for draft/send actions)."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: GmailInput = serde_json::from_value(input)?;
        let max = params.max_results.unwrap_or(10).min(50);

        // The connect action sets up the Composio managed backend by opening a
        // browser OAuth screen for the user to approve. It runs before the
        // is_configured gate so it can establish the very first connection.
        if params.action == "connect" {
            if !self.client.supports_connect() {
                return Ok(ToolOutput::new(
                    "The 'connect' action is only available with the Composio Gmail backend. \
                     Set JCODE_GMAIL_BACKEND=composio and COMPOSIO_API_KEY, then retry. \
                     For the default backend, run `jcode login google` instead.",
                ));
            }
            let no_browser = crate::auth::browser_suppressed(false);
            match self.client.connect(!no_browser).await {
                Ok(conn) => {
                    let who = conn
                        .email
                        .clone()
                        .unwrap_or_else(|| "your Gmail account".to_string());
                    return Ok(ToolOutput::new(format!(
                        "Gmail connected via Composio for {}. You can now search, read, draft, and send email.",
                        who
                    )));
                }
                Err(e) => {
                    return Ok(ToolOutput::new(format!("Gmail connect failed: {}", e)));
                }
            }
        }

        if !self.client.is_configured() {
            return Ok(ToolOutput::new(self.client.not_configured_message()));
        }

        if self.client.needs_connection() {
            return Ok(ToolOutput::new(
                "Gmail (Composio backend) has no connected account yet. Run the gmail tool with \
                 action 'connect' to authorize your Gmail account, then retry.",
            ));
        }

        match params.action.as_str() {
            "search" | "list" => {
                let query = params.query.as_deref();
                let label_refs: Vec<&str> = params
                    .label_ids
                    .as_ref()
                    .map(|v| v.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                let labels = if label_refs.is_empty() {
                    None
                } else {
                    Some(label_refs.as_slice())
                };

                let list = self.client.list_messages(query, labels, max).await?;
                retain_received(&ctx, "list_messages", &list).await?;
                let mut received = json!({"list":list,"messages":[]});
                let msgs = list.messages.unwrap_or_default();

                if msgs.is_empty() {
                    return Ok(ToolOutput::new("No messages found.")
                        .with_metadata(json!({"gmail_result":received})));
                }

                let mut results = Vec::new();
                for (i, msg_ref) in msgs.iter().enumerate().take(max as usize) {
                    match self
                        .client
                        .get_message(&msg_ref.id, MessageFormat::Metadata)
                        .await
                    {
                        Ok(msg) => {
                            retain_received(&ctx, "message_metadata", &msg).await?;
                            received["messages"]
                                .as_array_mut()
                                .unwrap()
                                .push(serde_json::to_value(&msg)?);
                            results.push(format!(
                                "{}. {}\n   From: {}\n   Date: {}\n   ID: {}",
                                i + 1,
                                msg.subject().unwrap_or("(no subject)"),
                                msg.from().unwrap_or("(unknown)"),
                                msg.date().unwrap_or(""),
                                msg.id,
                            ));
                        }
                        Err(e) => {
                            results.push(format!(
                                "{}. [error fetching {}: {}]",
                                i + 1,
                                msg_ref.id,
                                e
                            ));
                        }
                    }
                }

                let header = if let Some(q) = query {
                    format!("Search results for \"{}\" ({} found):", q, msgs.len())
                } else {
                    format!("Recent messages ({} shown):", results.len())
                };

                Ok(
                    ToolOutput::new(format!("{}\n\n{}", header, results.join("\n\n")))
                        .with_metadata(json!({"gmail_result":received})),
                )
            }

            "read" => {
                let id = params
                    .message_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("message_id is required for read action"))?;

                let msg = self.client.get_message(id, MessageFormat::Full).await?;
                retain_received(&ctx, "message_full", &msg).await?;
                Ok(ToolOutput::new(gmail::format_message_full(&msg))
                    .with_metadata(json!({"gmail_result":msg})))
            }

            "threads" => {
                let query = params.query.as_deref();
                let list = self.client.list_threads(query, max).await?;
                retain_received(&ctx, "list_threads", &list).await?;
                let received = serde_json::to_value(&list)?;
                let threads = list.threads.unwrap_or_default();

                if threads.is_empty() {
                    return Ok(ToolOutput::new("No threads found.")
                        .with_metadata(json!({"gmail_result":received})));
                }

                let mut results = Vec::new();
                for (i, t) in threads.iter().enumerate() {
                    results.push(format!(
                        "{}. {}\n   ID: {}",
                        i + 1,
                        t.snippet.as_deref().unwrap_or("(no snippet)"),
                        t.id,
                    ));
                }

                Ok(ToolOutput::new(format!(
                    "Threads ({}):\n\n{}",
                    threads.len(),
                    results.join("\n\n")
                ))
                .with_metadata(json!({"gmail_result":received})))
            }

            "thread" => {
                let id = params
                    .thread_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("thread_id is required for thread action"))?;

                // Accept a message ID too: if the thread lookup fails, try
                // resolving the ID as a message and use its containing thread.
                let thread = match self.client.get_thread(id).await {
                    Ok(t) => t,
                    Err(thread_err) => {
                        match self.client.get_message(id, MessageFormat::Metadata).await {
                            Ok(msg) => {
                                let tid = msg.thread_id.ok_or(thread_err)?;
                                self.client.get_thread(&tid).await?
                            }
                            Err(_) => return Err(thread_err),
                        }
                    }
                };
                retain_received(&ctx, "thread_full", &thread).await?;
                format_thread_result(thread)
            }

            "labels" => {
                let labels = self.client.list_labels().await?;
                retain_received(&ctx, "labels", &labels).await?;
                let mut results = Vec::new();
                for label in &labels {
                    let unread = label
                        .messages_unread
                        .map(|u| format!(" ({} unread)", u))
                        .unwrap_or_default();
                    let total = label
                        .messages_total
                        .map(|t| format!(" [{} total]", t))
                        .unwrap_or_default();
                    results.push(format!(
                        "- {} (id: {}){}{}",
                        label.name, label.id, unread, total
                    ));
                }
                Ok(ToolOutput::new(format!("Labels:\n{}", results.join("\n")))
                    .with_metadata(json!({"gmail_result":labels})))
            }

            "draft" => {
                let to = params
                    .to
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("'to' is required for draft action"))?;
                let subject = params.subject.as_deref().unwrap_or("");
                let body = params.body.as_deref().unwrap_or("");

                let attachments: Vec<std::path::PathBuf> = params
                    .attachments
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect();
                for path in &attachments {
                    if !path.is_file() {
                        return Ok(ToolOutput::new(format!(
                            "Attachment not found or not a file: {}",
                            path.display()
                        )));
                    }
                }

                let draft = self
                    .client
                    .create_draft_with_attachments(
                        to,
                        subject,
                        body,
                        params.in_reply_to.as_deref(),
                        params.thread_id.as_deref(),
                        &attachments,
                    )
                    .await?;

                retain_received(&ctx, "created_draft", &draft).await?;
                let attach_line = if attachments.is_empty() {
                    String::new()
                } else {
                    format!(
                        "Attachments ({}):\n{}\n",
                        attachments.len(),
                        attachments
                            .iter()
                            .map(|p| format!("  - {}", p.display()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                };
                Ok(ToolOutput::new(format!(
                    "Draft created successfully.\nDraft ID: {}\nTo: {}\nSubject: {}\n{}\nTo send this draft, use action 'send_draft' with draft_id '{}' and confirmed: true.",
                    draft.id, to, subject, attach_line, draft.id
                )).with_metadata(json!({"gmail_result":draft})))
            }

            "send" => {
                if !self.client.can_send() {
                    return Ok(ToolOutput::new(
                        "Send is not available. Your Gmail access is configured as Read & Draft Only (API-level restriction).\n\
                         The draft has been created - open Gmail to send it manually.\n\
                         To enable sending, rerun `jcode login google --google-access-tier full`.",
                    ));
                }

                let to = params
                    .to
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("'to' is required for send action"))?;
                let subject = params.subject.as_deref().unwrap_or("");
                let body = params.body.as_deref().unwrap_or("");

                let attachments: Vec<std::path::PathBuf> = params
                    .attachments
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect();
                for path in &attachments {
                    if !path.is_file() {
                        return Ok(ToolOutput::new(format!(
                            "Attachment not found or not a file: {}",
                            path.display()
                        )));
                    }
                }

                if params.confirmed != Some(true) {
                    let attach_line = if attachments.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "Attachments:\n{}\n",
                            attachments
                                .iter()
                                .map(|p| format!("  - {}", p.display()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    };
                    return Ok(ToolOutput::new(format!(
                        "CONFIRMATION REQUIRED: Send this email?\n\n\
                         To: {}\n\
                         Subject: {}\n\
                         {}\
                         Body:\n{}\n\n\
                         To confirm, call gmail again with the same parameters and confirmed: true.",
                        to, subject, attach_line, body
                    )));
                }

                let msg = self
                    .client
                    .send_message_with_attachments(
                        to,
                        subject,
                        body,
                        params.in_reply_to.as_deref(),
                        params.thread_id.as_deref(),
                        &attachments,
                    )
                    .await?;

                retain_received(&ctx, "sent_message", &msg).await?;
                Ok(ToolOutput::new(format!(
                    "Email sent successfully.\nMessage ID: {}\nTo: {}\nSubject: {}\nAttachments: {}",
                    msg.id,
                    to,
                    subject,
                    attachments.len()
                )).with_metadata(json!({"gmail_result":msg})))
            }

            "send_draft" => {
                if !self.client.can_send() {
                    return Ok(ToolOutput::new(
                        "Send is not available. Your Gmail access is configured as Read & Draft Only (API-level restriction).\n\
                         Open Gmail to send the draft manually.\n\
                         To enable sending, rerun `jcode login google --google-access-tier full`.",
                    ));
                }

                let draft_id = params.draft_id.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("'draft_id' is required for send_draft action")
                })?;

                if params.confirmed != Some(true) {
                    return Ok(ToolOutput::new(format!(
                        "CONFIRMATION REQUIRED: Send draft {}?\n\n\
                         To confirm, call gmail again with action 'send_draft', draft_id '{}', and confirmed: true.",
                        draft_id, draft_id
                    )));
                }

                let msg = self.client.send_draft(draft_id).await?;
                retain_received(&ctx, "sent_draft", &msg).await?;
                Ok(
                    ToolOutput::new(format!("Draft sent successfully.\nMessage ID: {}", msg.id))
                        .with_metadata(json!({"gmail_result":msg})),
                )
            }

            "trash" => {
                if !self.client.can_delete() {
                    return Ok(ToolOutput::new(
                        "Trash is not available. Your Gmail access is configured as Read & Draft Only (API-level restriction).\n\
                         To enable delete, rerun `jcode login google --google-access-tier full`.",
                    ));
                }

                let id = params
                    .message_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("'message_id' is required for trash action"))?;

                if params.confirmed != Some(true) {
                    return Ok(ToolOutput::new(format!(
                        "CONFIRMATION REQUIRED: Move message {} to trash?\n\n\
                         To confirm, call gmail again with action 'trash', message_id '{}', and confirmed: true.",
                        id, id
                    )));
                }

                self.client.trash_message(id).await?;
                Ok(ToolOutput::new(format!("Message {} moved to trash.", id)))
            }

            "modify_labels" => {
                let id = params
                    .message_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("'message_id' is required for modify_labels"))?;

                let add: Vec<&str> = params
                    .add_labels
                    .as_ref()
                    .map(|v| v.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                let remove: Vec<&str> = params
                    .remove_labels
                    .as_ref()
                    .map(|v| v.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();

                self.client.modify_labels(id, &add, &remove).await?;
                Ok(ToolOutput::new(format!(
                    "Labels modified on message {}.\nAdded: {:?}\nRemoved: {:?}",
                    id, add, remove
                )))
            }

            other => Ok(ToolOutput::new(format!(
                "Unknown gmail action: '{}'. Valid actions: search, read, list, draft, send, send_draft, threads, thread, labels, trash, modify_labels",
                other
            ))),
        }
    }
}

fn format_thread_result(thread: gmail::Thread) -> Result<ToolOutput> {
    let received = serde_json::to_value(&thread)?;
    let thread_id = thread.id.clone();
    let messages = thread.messages.unwrap_or_default();

    if messages.is_empty() {
        return Ok(ToolOutput::new("Thread has no messages.")
            .with_metadata(json!({"gmail_result":received})));
    }

    let mut results = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        let mut entry = format!(
            "--- Message {} ---\nID: {}\nFrom: {}\nDate: {}\nSubject: {}\nSnippet: {}",
            i + 1,
            msg.id,
            msg.from().unwrap_or("(unknown)"),
            msg.date().unwrap_or(""),
            msg.subject().unwrap_or("(no subject)"),
            msg.snippet.as_deref().unwrap_or(""),
        );
        let attachments = msg.attachments();
        if !attachments.is_empty() {
            entry.push_str(&format!(
                "\nAttachments ({}):\n{}",
                attachments.len(),
                gmail::format_attachment_lines(&attachments)
            ));
        }
        results.push(entry);
    }

    Ok(ToolOutput::new(format!(
        "Thread {} ({} messages):\n\n{}",
        thread_id,
        messages.len(),
        results.join("\n\n")
    ))
    .with_metadata(json!({"gmail_result":received})))
}

async fn retain_received(
    ctx: &ToolContext,
    operation: &str,
    response: &impl serde::Serialize,
) -> Result<()> {
    if let Some(capture) = ctx.invocation.capture.clone() {
        let mut bytes = serde_json::to_vec(&json!({"operation":operation,"response":response}))?;
        bytes.push(b'\n');
        tokio::task::spawn_blocking(move || {
            for chunk in bytes.chunks(64 * 1024) {
                capture.append_part("gmail-responses", chunk)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Capture, ExecutionStore, Invocation, PreparedInvocation, RunState};
    use std::sync::Arc;

    #[tokio::test]
    async fn full_received_thread_and_prior_acquisitions_survive_summary_rendering() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let input = Invocation {
            session_id: "gmail-fixture".into(),
            message_id: "m".into(),
            call_path: vec!["call".into()],
            tool: "gmail".into(),
            input: json!({"action":"thread"}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let capture = Arc::new(Capture::create(
            store.clone(),
            record.clone(),
            Default::default(),
        )?);
        let ctx = ToolContext {
            session_id: input.session_id,
            message_id: input.message_id,
            tool_call_id: "call".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
            invocation: jcode_tool_core::InvocationContext {
                capture: Some(capture.clone()),
                ..Default::default()
            },
        };
        let long = "Z".repeat(80_000) + "BODY_TAIL";
        let thread: gmail::Thread = serde_json::from_value(
            json!({"id":"t","messages":[{"id":"m","snippet":"selected summary","payload":{"mimeType":"text/plain","headers":[{"name":"X-Complete","value":long}],"body":{"data":long,"size":long.len()}}}]}),
        )?;
        retain_received(&ctx, "thread_full", &thread).await?;
        let output = format_thread_result(thread)?;
        assert!(output.output.contains("selected summary"));
        assert!(!output.output.contains("BODY_TAIL"));
        assert_eq!(
            output.metadata.as_ref().unwrap()["gmail_result"]["messages"][0]["payload"]["body"]["data"],
            long
        );
        // Emulate a later request failing. Previously acquired structures still exist.
        let mut failure = ToolOutput::new("later metadata request failed").with_error(true);
        failure.metadata = output.metadata;
        capture.seal(failure, RunState::Failed)?;
        let record = store.inspect(&record.id)?.unwrap();
        let body = record
            .output_path
            .as_ref()
            .unwrap()
            .with_file_name("part-gmail-responses.bin");
        let acquired: serde_json::Value = serde_json::from_slice(&std::fs::read(body)?)?;
        assert_eq!(
            acquired["response"]["messages"][0]["payload"]["body"]["data"],
            long
        );
        let restored = store.result(&record, std::num::NonZeroUsize::new(100).unwrap())?;
        assert!(restored.is_error);
        assert_eq!(
            restored.metadata.unwrap()["gmail_result"]["messages"][0]["payload"]["headers"][0]["value"],
            long
        );
        Ok(())
    }
}
