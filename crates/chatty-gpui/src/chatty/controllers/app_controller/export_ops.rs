use super::*;

fn push_markdown_code_block(md: &mut String, language: &str, body: &str) {
    if body.trim().is_empty() {
        return;
    }

    md.push_str(&format!("```{language}\n{body}\n```\n\n"));
}

fn push_system_trace_markdown(md: &mut String, trace_json: &serde_json::Value) {
    match serde_json::from_value::<SystemTrace>(trace_json.clone()) {
        Ok(trace) if trace.has_items() => {
            md.push_str("### Trace\n\n");

            for (index, item) in trace.items.iter().enumerate() {
                match item {
                    TraceItem::Thinking(thinking) => {
                        let status = match thinking.state {
                            ThinkingState::Processing => "running",
                            ThinkingState::Completed => "completed",
                        };
                        md.push_str(&format!("{}. **Thinking** ({status})\n", index + 1));

                        if !thinking.summary.trim().is_empty() {
                            md.push_str(&format!("   - Summary: {}\n", thinking.summary.trim()));
                        }

                        if !thinking.content.trim().is_empty() {
                            md.push_str("   - Details:\n\n");
                            push_markdown_code_block(md, "text", thinking.content.trim());
                        } else {
                            md.push('\n');
                        }
                    }
                    TraceItem::ToolCall(tool_call) => {
                        let status = match &tool_call.state {
                            ToolCallState::Running => "running".to_string(),
                            ToolCallState::Success => "success".to_string(),
                            ToolCallState::Error(err) => format!("error: {err}"),
                        };

                        md.push_str(&format!(
                            "{}. **Tool:** `{}` ({status})\n",
                            index + 1,
                            tool_call.display_name
                        ));

                        if !tool_call.input.trim().is_empty() {
                            md.push_str("   - Input:\n\n");
                            push_markdown_code_block(md, "text", tool_call.input.trim());
                        }

                        if let Some(output) = tool_call.output.as_deref()
                            && !output.trim().is_empty()
                        {
                            md.push_str("   - Output:\n\n");
                            push_markdown_code_block(md, "text", output.trim());
                        } else if let Some(output_preview) = tool_call.output_preview.as_deref()
                            && !output_preview.trim().is_empty()
                        {
                            md.push_str("   - Output Preview:\n\n");
                            push_markdown_code_block(md, "text", output_preview.trim());
                        } else {
                            md.push('\n');
                        }
                    }
                    TraceItem::ApprovalPrompt(approval) => {
                        let status = match approval.state {
                            ApprovalState::Pending => "pending",
                            ApprovalState::Approved => "approved",
                            ApprovalState::Denied => "denied",
                        };

                        md.push_str(&format!(
                            "{}. **Approval** ({status})\n   - Command: `{}`\n\n",
                            index + 1,
                            approval.command
                        ));
                    }
                }
            }
        }
        Ok(_) => {}
        Err(error) => {
            warn!(error = ?error, "Failed to deserialize trace for markdown export");
            md.push_str("### Trace (raw)\n\n");
            match serde_json::to_string_pretty(trace_json) {
                Ok(raw_json) => push_markdown_code_block(md, "json", &raw_json),
                Err(error) => {
                    warn!(error = ?error, "Failed to pretty-print raw trace JSON for export");
                }
            }
        }
    }
}

impl ChattyApp {
    /// Export a conversation as Markdown with an OS file-save dialog.
    ///
    /// Builds markdown from the conversation history in `ConversationsStore`,
    /// prompts the user for a save location, and writes the file asynchronously.
    pub(super) fn export_conversation_markdown(&self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();

        // Build markdown from ConversationsStore (works for any conversation, not just active)
        let store = cx.global::<ConversationsStore>();
        let Some(conv) = store.get_conversation(&conv_id) else {
            warn!(conv_id = %conv_id, "Cannot export: conversation not found or has no messages");
            return;
        };

        let title = conv.title().to_string();
        let mut markdown = format!("# {title}\n\n");
        for entry in conv.entries() {
            let trace_json = entry.system_trace.as_ref();

            match &entry.message {
                rig::completion::Message::User { content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            rig::completion::message::UserContent::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.is_empty() {
                        markdown.push_str(&format!("---\n\n**User**\n\n{text}\n\n"));
                    }
                }
                rig::completion::Message::Assistant { content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            rig::completion::message::AssistantContent::Text(t) => {
                                Some(t.text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.is_empty() || trace_json.is_some() {
                        markdown.push_str("---\n\n**Assistant**\n\n");

                        if !text.is_empty() {
                            markdown.push_str(&text);
                            markdown.push_str("\n\n");
                        }

                        if let Some(trace_json) = trace_json {
                            push_system_trace_markdown(&mut markdown, trace_json);
                        }
                    }
                }
            }
        }

        if markdown.is_empty() {
            warn!(conv_id = %conv_id, "Cannot export: conversation not found or has no messages");
            return;
        }

        let suggested = format!(
            "{}.md",
            title.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
        );
        let home = dirs::home_dir()
            .unwrap_or_else(|| dirs::document_dir().unwrap_or_else(|| PathBuf::from(".")));

        cx.spawn(async move |_weak, cx| {
            let receiver = cx
                .update(|cx| cx.prompt_for_new_path(&home, Some(&suggested)))
                .map_err(|e| warn!(error = ?e, "Failed to open save dialog"))
                .ok()?;
            match receiver.await {
                Ok(Ok(Some(path))) => {
                    if let Err(e) = tokio::fs::write(&path, markdown.as_bytes()).await {
                        warn!(error = ?e, path = ?path, "Failed to write markdown export");
                    }
                }
                Ok(Ok(None)) => {} // user cancelled
                Ok(Err(e)) => warn!(error = ?e, "Save dialog returned error"),
                Err(e) => warn!(error = ?e, "Failed to receive save dialog result"),
            }
            Some(())
        })
        .detach();
    }

    /// Export a conversation as ATIF JSON to the exports directory.
    ///
    /// Builds ConversationData from the store, looks up the ModelConfig for
    /// provider metadata, converts to ATIF, and writes the file asynchronously.
    pub(super) fn export_conversation_atif(&self, conv_id: &str, cx: &mut Context<Self>) {
        let conv_id = conv_id.to_string();

        // Build ConversationData and get the model config (same data as persist_conversation)
        let export_data = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store
                .get_conversation(&conv_id)
                .and_then(build_conversation_data)
        });

        let Some(conv_data) = export_data else {
            warn!(conv_id = %conv_id, "Cannot export ATIF: conversation not found");
            return;
        };

        // Look up ModelConfig for provider metadata
        let model_config: Option<ModelConfig> = cx
            .global::<ModelsModel>()
            .get_model(&conv_data.model_id)
            .cloned();

        cx.spawn(async move |_, _cx| {
            // Convert to ATIF
            let atif_json = match conversation_to_atif(&conv_data, model_config.as_ref()) {
                Ok(json) => json,
                Err(e) => {
                    warn!(error = ?e, conv_id = %conv_id, "Failed to convert conversation to ATIF");
                    return Ok::<_, anyhow::Error>(());
                }
            };

            // Determine exports directory
            let exports_dir = match dirs::config_dir() {
                Some(config) => config.join("chatty").join("exports"),
                None => {
                    warn!("Cannot determine config directory for ATIF export");
                    return Ok(());
                }
            };

            // Create exports directory if needed
            if let Err(e) = tokio::fs::create_dir_all(&exports_dir).await {
                warn!(error = ?e, "Failed to create ATIF exports directory");
                return Ok(());
            }

            // Write atomically using temp file + rename
            let file_path = exports_dir.join(format!("{}.atif.json", conv_id));
            let temp_path = file_path.with_extension(format!("json.{}.tmp", std::process::id()));

            let json_str = match serde_json::to_string_pretty(&atif_json) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = ?e, conv_id = %conv_id, "Failed to serialize ATIF JSON");
                    return Ok(());
                }
            };

            if let Err(e) = tokio::fs::write(&temp_path, &json_str).await {
                warn!(error = ?e, conv_id = %conv_id, "Failed to write ATIF temp file");
                return Ok(());
            }

            if let Err(e) = tokio::fs::rename(&temp_path, &file_path).await {
                warn!(error = ?e, conv_id = %conv_id, "Failed to rename ATIF temp file");
                return Ok(());
            }

            debug!(
                conv_id = %conv_id,
                path = %file_path.display(),
                "ATIF export saved"
            );

            Ok(())
        })
        .detach();
    }

    /// Export a conversation as JSONL (SFT + DPO) to the exports directory.
    ///
    /// Builds ConversationData from the store, converts to SFT and DPO JSONL lines,

    /// and appends to sft.jsonl and dpo.jsonl with deduplication by _conversation_id.
    pub(super) fn export_conversation_jsonl(&self, conv_id: &str, cx: &mut Context<Self>) {
        let conv_id = conv_id.to_string();

        // Build ConversationData (same pattern as export_conversation_atif)
        let export_data = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store
                .get_conversation(&conv_id)
                .and_then(build_conversation_data)
        });

        let Some(conv_data) = export_data else {
            warn!(conv_id = %conv_id, "Cannot export JSONL: conversation not found");
            return;
        };

        // Look up ModelConfig for system prompt
        let model_config: Option<ModelConfig> = cx
            .global::<ModelsModel>()
            .get_model(&conv_data.model_id)
            .cloned();

        cx.spawn(async move |_, _cx| {
            // Convert to SFT
            let sft_options = SftExportOptions::default();
            let sft_line =
                match conversation_to_sft_jsonl(&conv_data, model_config.as_ref(), &sft_options) {
                    Ok(line) => line,
                    Err(e) => {
                        warn!(error = ?e, conv_id = %conv_id, "Failed to convert conversation to SFT JSONL");
                        None
                    }
                };

            // Convert to DPO
            let dpo_lines = match conversation_to_dpo_jsonl(&conv_data, model_config.as_ref()) {
                Ok(lines) => lines,
                Err(e) => {
                    warn!(error = ?e, conv_id = %conv_id, "Failed to convert conversation to DPO JSONL");
                    Vec::new()
                }
            };

            // Determine exports directory
            let exports_dir = match dirs::config_dir() {
                Some(config) => config.join("chatty").join("exports"),
                None => {
                    warn!("Cannot determine config directory for JSONL export");
                    return Ok::<_, anyhow::Error>(());
                }
            };

            if let Err(e) = tokio::fs::create_dir_all(&exports_dir).await {
                warn!(error = ?e, "Failed to create JSONL exports directory");
                return Ok(());
            }

            // Append SFT line with dedup
            let has_sft = sft_line.is_some();
            if let Some(sft_val) = sft_line
                && let Err(e) = append_jsonl_with_dedup(
                    &exports_dir.join("sft.jsonl"),
                    &[sft_val],
                    &conv_id,
                )
                .await
            {
                warn!(error = ?e, conv_id = %conv_id, "Failed to write SFT JSONL");
            }

            // Append DPO lines with dedup
            let dpo_count = dpo_lines.len();
            if !dpo_lines.is_empty()
                && let Err(e) = append_jsonl_with_dedup(
                    &exports_dir.join("dpo.jsonl"),
                    &dpo_lines,
                    &conv_id,
                )
                .await
            {
                warn!(error = ?e, conv_id = %conv_id, "Failed to write DPO JSONL");
            }

            debug!(
                conv_id = %conv_id,
                has_sft = has_sft,
                dpo_count = dpo_count,
                "JSONL export saved"
            );

            Ok(())
        })
        .detach();
    }
}

