//! The pull-request status bar that sits directly above the composer.
//!
//! # What lives here
//!
//! - `PrStatusBarView` entity — the resolved `PullRequestSummary` for the
//!   conversation's workspace, the poller that keeps it fresh, and the
//!   per-conversation dismissal.
//! - Its `Render` impl: PR number, repo, branch, diff stats, a CI pill with
//!   a popover listing every check, and a dismiss button.
//!
//! # What does NOT live here
//!
//! - Resolving the PR itself — `chatty_core::services::github_pr_service`.
//! - Deciding *which* workspace is current — `ChatView::sync_pr_status`
//!   feeds that in via `set_context` on every frame.
//!
//! The bar is invisible unless a PR is resolved, so a workspace that is not
//! a git checkout, has no GitHub remote, or has no PR for its branch renders
//! nothing at all.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex};

use crate::assets::CustomIcon;
use chatty_core::services::github_pr_service::{
    CheckState, PrState, PullRequestSummary, resolve_pull_request,
};

/// Poll cadence once CI has settled.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Faster cadence while checks are still running.
const PENDING_POLL_INTERVAL: Duration = Duration::from_secs(20);

pub struct PrStatusBarView {
    /// Conversation the bar currently belongs to; a switch re-resolves.
    conversation_id: Option<String>,
    /// Workspace whose branch the PR is looked up for.
    workspace: Option<PathBuf>,
    pr: Option<PullRequestSummary>,
    /// The (conversation, PR number) pair the user dismissed. A different
    /// conversation or a different PR brings the bar back.
    dismissed: Option<(Option<String>, u64)>,
    /// Bumped on every context change so a poll that is already in flight
    /// cannot write its stale result over the new one.
    generation: u64,
    /// Held so the poll loop stays alive; dropped when the context changes.
    poll_task: Option<Task<()>>,
}

impl Default for PrStatusBarView {
    fn default() -> Self {
        Self::new()
    }
}

impl PrStatusBarView {
    pub fn new() -> Self {
        Self {
            conversation_id: None,
            workspace: None,
            pr: None,
            dismissed: None,
            generation: 0,
            poll_task: None,
        }
    }

    /// Point the bar at a conversation and its effective workspace.
    ///
    /// Called once per frame; everything after the equality check only runs
    /// when the conversation or the workspace actually changed.
    pub fn set_context(
        &mut self,
        conversation_id: Option<String>,
        workspace: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.conversation_id == conversation_id && self.workspace == workspace {
            return;
        }

        self.conversation_id = conversation_id;
        self.workspace = workspace;
        self.pr = None;
        self.generation += 1;
        self.poll_task = None;

        if self.workspace.is_some() {
            self.start_polling(cx);
        }
        cx.notify();
    }

    /// Resolve the PR now, then keep re-resolving while this context is live.
    ///
    /// Re-reading the branch on every pass is what makes a `switch_branch` or
    /// an external checkout show up without any explicit notification.
    fn start_polling(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let generation = self.generation;

        self.poll_task = Some(cx.spawn(async move |entity, cx| {
            loop {
                let summary = resolve_pull_request(&workspace).await;

                let next_interval = entity.update(cx, |this, cx| {
                    if this.generation != generation {
                        return None;
                    }
                    this.pr = summary;
                    cx.notify();
                    Some(this.poll_interval())
                });

                match next_interval {
                    Ok(Some(interval)) => cx.background_executor().timer(interval).await,
                    // Superseded by a newer context, or the view is gone.
                    _ => break,
                }
            }
        }));
    }

    fn poll_interval(&self) -> Duration {
        match self.pr.as_ref().and_then(|pr| pr.checks_state()) {
            Some(CheckState::Pending) => PENDING_POLL_INTERVAL,
            _ => IDLE_POLL_INTERVAL,
        }
    }

    /// The PR to render, or `None` when there is none or the user hid it.
    fn visible_pr(&self) -> Option<&PullRequestSummary> {
        let pr = self.pr.as_ref()?;
        match &self.dismissed {
            Some((conversation_id, number))
                if conversation_id == &self.conversation_id && *number == pr.number =>
            {
                None
            }
            _ => Some(pr),
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(pr) = self.pr.as_ref() {
            self.dismissed = Some((self.conversation_id.clone(), pr.number));
            cx.notify();
        }
    }
}

/// Open a URL in the user's default browser.
fn open_in_browser(url: &str) {
    let result = {
        #[cfg(target_os = "macos")]
        {
            Command::new("open").arg(url).spawn()
        }
        #[cfg(target_os = "windows")]
        {
            Command::new("cmd").args(["/C", "start", "", url]).spawn()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Command::new("xdg-open").arg(url).spawn()
        }
    };
    if let Err(error) = result {
        tracing::warn!(error = ?error, "Failed to open pull request URL");
    }
}

fn state_icon(state: PrState) -> CustomIcon {
    match state {
        PrState::Merged => CustomIcon::GitMerge,
        _ => CustomIcon::GitPr,
    }
}

fn state_label(state: PrState) -> &'static str {
    match state {
        PrState::Open => "Open",
        PrState::Draft => "Draft",
        PrState::Merged => "Merged",
        PrState::Closed => "Closed",
    }
}

fn check_symbol(state: CheckState) -> &'static str {
    match state {
        CheckState::Pending => "•",
        CheckState::Passing => "✓",
        CheckState::Failing => "✗",
    }
}

impl Render for PrStatusBarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(pr) = self.visible_pr().cloned() else {
            return div().into_any_element();
        };

        let merged = pr.state == PrState::Merged;
        // Merged PRs get the accent tint; everything else stays muted so the
        // bar never competes with the composer for attention.
        let background = if merged {
            cx.theme().accent
        } else {
            cx.theme().secondary
        };

        // Every text target opens the same PR page.
        let open_pr = {
            let url = pr.url.clone();
            move |_event: &ClickEvent, _window: &mut Window, _cx: &mut App| open_in_browser(&url)
        };

        let checks_pill = pr.checks_state().map(|state| {
            let checks = pr.checks.clone();
            let label = match state {
                CheckState::Pending => "CI",
                CheckState::Passing => "CI passing",
                CheckState::Failing => "CI failing",
            };
            let trigger = Button::new("pr-checks")
                .ghost()
                .xsmall()
                .label(format!("{} {}", check_symbol(state), label))
                .icon(Icon::new(IconName::ChevronDown).size_3());

            Popover::new("pr-checks-menu")
                .trigger(trigger)
                .appearance(false)
                .content(move |_popover, _window, cx| {
                    // Theme lookups are hoisted: the hover closure below must
                    // be 'static, so it cannot borrow `cx`.
                    let background = cx.theme().background;
                    let border = cx.theme().border;
                    let foreground = cx.theme().foreground;
                    let hover_bg = cx.theme().muted;
                    let colors = (
                        cx.theme().muted_foreground,
                        cx.theme().success,
                        cx.theme().danger,
                    );
                    let checks = checks.clone();

                    div()
                        .flex()
                        .flex_col()
                        .bg(background)
                        .border_1()
                        .border_color(border)
                        .rounded_md()
                        .shadow_md()
                        .p_1()
                        .min_w(px(220.0))
                        .children(checks.into_iter().enumerate().map(move |(ix, check)| {
                            let url = check.url.clone();
                            let color = match check.state {
                                CheckState::Pending => colors.0,
                                CheckState::Passing => colors.1,
                                CheckState::Failing => colors.2,
                            };
                            div()
                                .id(ix)
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_xs()
                                .when(url.is_some(), |d| {
                                    d.cursor_pointer().hover(move |s| s.bg(hover_bg)).on_click(
                                        move |_event, _window, _cx| {
                                            if let Some(url) = url.as_deref() {
                                                open_in_browser(url);
                                            }
                                        },
                                    )
                                })
                                .child(div().text_color(color).child(check_symbol(check.state)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(foreground)
                                        .child(check.name),
                                )
                        }))
                })
        });

        let dismiss_button = Button::new("pr-dismiss")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::Close).size_3())
            .tooltip("Hide this pull request")
            .on_click(cx.listener(|this, _event, _window, cx| this.dismiss(cx)));

        h_flex()
            .id("pr-status-bar")
            .w_full()
            .min_w_0()
            .mb_2()
            .px_3()
            .py_1()
            .gap_2()
            .items_center()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(background)
            .text_xs()
            .child(
                Icon::new(state_icon(pr.state))
                    .size_3()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .id("pr-number")
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().foreground)
                    .child(format!("#{}", pr.number))
                    .tooltip({
                        let title = pr.title.clone();
                        move |window, cx| Tooltip::new(title.clone()).build(window, cx)
                    })
                    .on_click(open_pr.clone()),
            )
            .child(
                div()
                    .id("pr-repo")
                    .cursor_pointer()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(pr.repo_name().to_string())
                    .on_click(open_pr.clone()),
            )
            .child(
                div()
                    .id("pr-branch")
                    .cursor_pointer()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(pr.branch.clone())
                    .on_click(open_pr),
            )
            .when(pr.additions > 0 || pr.deletions > 0, |this| {
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{}", pr.additions)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("−{}", pr.deletions)),
                        ),
                )
            })
            .child(div().flex_grow())
            .when_some(checks_pill, |this, pill| this.child(pill))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(state_label(pr.state)),
            )
            .child(dismiss_button)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would drag in `gpui::test`, which shadows the
    // built-in `#[test]` attribute.
    use super::{IDLE_POLL_INTERVAL, PENDING_POLL_INTERVAL, PrStatusBarView};
    use chatty_core::services::github_pr_service::{
        CheckRun, CheckState, PrState, PullRequestSummary,
    };

    fn summary(number: u64) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: "Title".into(),
            state: PrState::Open,
            url: "https://github.com/o/r/pull/1".into(),
            repo: "o/r".into(),
            branch: "feature".into(),
            additions: 1,
            deletions: 2,
            checks: Vec::new(),
        }
    }

    fn view_with(pr: PullRequestSummary, conversation_id: &str) -> PrStatusBarView {
        let mut view = PrStatusBarView::new();
        view.conversation_id = Some(conversation_id.to_string());
        view.pr = Some(pr);
        view
    }

    #[test]
    fn dismissal_hides_only_that_conversation_and_pr() {
        let mut view = view_with(summary(42), "conv-a");
        assert!(view.visible_pr().is_some());

        view.dismissed = Some((Some("conv-a".into()), 42));
        assert!(view.visible_pr().is_none());

        // A new PR on the same conversation brings the bar back.
        view.pr = Some(summary(43));
        assert!(view.visible_pr().is_some());

        // So does the same PR seen from a different conversation.
        view.pr = Some(summary(42));
        view.conversation_id = Some("conv-b".into());
        assert!(view.visible_pr().is_some());
    }

    #[test]
    fn poll_interval_speeds_up_while_checks_are_pending() {
        let mut view = PrStatusBarView::new();
        assert_eq!(view.poll_interval(), IDLE_POLL_INTERVAL);

        let mut pr = summary(1);
        pr.checks = vec![CheckRun {
            name: "test".into(),
            state: CheckState::Pending,
            url: None,
        }];
        view.pr = Some(pr);
        assert_eq!(view.poll_interval(), PENDING_POLL_INTERVAL);

        let mut pr = summary(1);
        pr.checks = vec![CheckRun {
            name: "test".into(),
            state: CheckState::Passing,
            url: None,
        }];
        view.pr = Some(pr);
        assert_eq!(view.poll_interval(), IDLE_POLL_INTERVAL);
    }
}
