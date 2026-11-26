//! `/send-to-cloud` and `/continue-here` (HX-19, replacing `/online`): the
//! terminal's two hand-off commands, over chatty-core's `session::handoff`
//! (HX-14) so the desktop dialog and these say and do the same thing.
//!
//! Sending shows what goes and what stays before anything leaves this
//! machine, and goes only on `/send-to-cloud confirm`. Continuing here brings
//! data *down* and asks for no confirmation.

use anyhow::Context as _;
use chatty_core::services::hive_files::HiveAttachmentUploader;
use chatty_core::session::{
    CONTINUE_HERE_SUMMARY, HandoffError, HandoffServer, HandoffSummary, LOCAL_MODEL_NOTE,
    SEND_TO_CLOUD_SUMMARY, SendToCloud, continue_here, refuse_reason, send_to_cloud,
    this_device_id,
};

use super::ChatEngine;

/// Render a hand-off's "what travels and what does not" table for a terminal.
///
/// The strings come from chatty-core, so the TUI prompt and the desktop
/// dialog say the same thing and neither can drift from what the code does.
pub(super) fn push_summary(out: &mut String, summary: &HandoffSummary) {
    out.push_str(summary.lead);
    out.push_str(&format!("\n{}:\n", summary.goes_title));
    for item in summary.goes {
        out.push_str(&format!("  + {item}\n"));
    }
    out.push_str(&format!("{}:\n", summary.stays_title));
    for item in summary.stays {
        out.push_str(&format!("  - {item}\n"));
    }
}

impl ChatEngine {
    /// `/send-to-cloud [confirm [<cloud model id>]]`.
    pub async fn send_to_cloud_command(&mut self, arg: Option<&str>) {
        let message = match self.send_to_cloud(arg).await {
            Ok(text) => text,
            Err(error) => format!("{error:#}"),
        };
        self.add_system_message(message);
    }

    async fn send_to_cloud(&mut self, arg: Option<&str>) -> anyhow::Result<String> {
        let sync = self.conversation_sync();
        if let Some(refusal) = refuse_reason(self.is_streaming, &sync, true) {
            return Ok(refusal.to_string());
        }
        let mut words = arg.unwrap_or_default().split_whitespace();
        if words.next() != Some("confirm") {
            let mut out = String::new();
            push_summary(&mut out, &SEND_TO_CLOUD_SUMMARY);
            out.push_str(
                "\nRun /send-to-cloud confirm to send it (add a cloud model id to pick one).",
            );
            return Ok(out);
        }
        let cloud_model = words.next().map(str::to_string);

        let server = self.handoff_server().await?;
        let conversation = self
            .session
            .conversation()
            .context("There is no conversation to send.")?;
        let data = conversation.snapshot().to_data()?;
        let sent = send_to_cloud(
            &server,
            SendToCloud {
                conversation: &data,
                sync: &sync,
                cloud_model: cloud_model.as_deref(),
            },
            &HiveAttachmentUploader::new(server.url(), self.cloud.auth.clone()),
        )
        .await;
        let new_sync = match sent {
            Ok(sync) => sync,
            Err(HandoffError::NeedsCloudModel(models)) => {
                let mut out = format!("{LOCAL_MODEL_NOTE}\n");
                let suggested = models
                    .preselected(&data.model_id)
                    .map(|model| model.id.clone());
                for model in &models.models {
                    out.push_str(&format!("  {}  {}\n", model.id, model.name));
                }
                out.push_str("Run /send-to-cloud confirm <model id>");
                if let Some(id) = suggested {
                    out.push_str(&format!(" (for example {id})"));
                }
                out.push('.');
                return Ok(out);
            }
            Err(error) => return Ok(error.to_string()),
        };

        // Only a send the server accepted changes anything here.
        if let Some((url, remote_id)) = new_sync.cloud_on() {
            let auth = self.cloud.auth.clone();
            self.hosted = Some(self.new_hosted(url, remote_id, auth));
        }
        if let Some(conversation) = self.session.conversation_mut() {
            conversation.set_sync(new_sync);
        }
        self.persist_synced();
        Ok(
            "This conversation now runs in the cloud. Its history stays on this computer too; \
            /continue-here brings it back."
                .to_string(),
        )
    }

    /// `/continue-here`: take the conversation over on this device.
    pub async fn continue_here_command(&mut self) {
        let message = match self.continue_here().await {
            Ok(text) => text,
            Err(error) => format!("{error:#}"),
        };
        self.add_system_message(message);
    }

    async fn continue_here(&mut self) -> anyhow::Result<String> {
        let sync = self.conversation_sync();
        if let Some(refusal) = refuse_reason(self.is_streaming, &sync, false) {
            return Ok(refusal.to_string());
        }
        let server = self.handoff_server().await?;
        let device_id = match self.this_device(&server).await {
            Ok(id) => id,
            Err(error) => return Ok(error.to_string()),
        };
        let here = match continue_here(&server, &sync, &device_id).await {
            Ok(here) => here,
            Err(error) => return Ok(error.to_string()),
        };
        let local_id = self
            .session
            .conversation()
            .map(|conversation| conversation.id().to_string())
            .context("There is no conversation to continue.")?;
        let conversation = self
            .session
            .conversation_mut()
            .context("There is no conversation to continue.")?;
        conversation
            .replace_from_remote(here.remote.to_conversation_data(&local_id))
            .context("Could not adopt the cloud copy of the conversation.")?;
        conversation.set_sync(here.sync);
        self.hosted = None;
        self.persist_synced();
        self.show_conversation();
        let mut out = String::from("This conversation now runs on this computer.\n");
        push_summary(&mut out, &CONTINUE_HERE_SUMMARY);
        Ok(out.trim_end().to_string())
    }

    /// This device's id, asked once per sign-in and kept.
    async fn this_device(&mut self, server: &HandoffServer) -> Result<String, HandoffError> {
        if let Some(id) = self.cloud.this_device.clone() {
            return Ok(id);
        }
        let id = this_device_id(server).await?;
        self.cloud.this_device = Some(id.clone());
        Ok(id)
    }

    /// The Hive server hand-offs talk to, signed in as the user.
    async fn handoff_server(&mut self) -> anyhow::Result<HandoffServer> {
        self.cloud_connect().await;
        match self.cloud.hive_url.as_deref() {
            Some(url) => Ok(HandoffServer::new(url, self.cloud.auth.clone())),
            None => anyhow::bail!(chatty_core::tui_hive::RUN_LOGIN),
        }
    }
}
