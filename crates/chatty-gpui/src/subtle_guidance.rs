use std::time::Duration;

use crate::chatty::views::message_types::{SystemTrace, ToolCallState, TraceItem};

#[allow(dead_code)]
pub const PHASE_ANTI_FLICKER: Duration = Duration::from_millis(250);
#[allow(dead_code)]
pub const PRE_STREAM_ANTI_NOISE: Duration = Duration::from_millis(600);
pub const GUIDANCE_BREATHE_PERIOD: Duration = Duration::from_millis(1600);
pub const SKELETON_SHIMMER_PERIOD: Duration = Duration::from_millis(1500);
#[allow(dead_code)]
pub const STREAM_SMOOTH_BASELINE_CPS: usize = 30;
#[allow(dead_code)]
pub const STREAM_SMOOTH_MAX_CPS: usize = 90;
#[allow(dead_code)]
pub const STREAM_SMOOTH_ACCELERATE_AFTER: usize = 80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendPhase {
    RecallingContext,
    ChoosingTools,
    Thinking,
}

impl BackendPhase {
    pub fn copy(self) -> &'static str {
        match self {
            Self::RecallingContext => "Recalling context",
            Self::ChoosingTools => "Choosing tools",
            Self::Thinking => "Thinking",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MotionMode {
    Full,
    Reduced,
}

pub fn use_reduced_motion() -> bool {
    std::env::var("CHATTY_REDUCED_MOTION")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[allow(dead_code)]
pub fn motion_mode() -> MotionMode {
    if use_reduced_motion() {
        MotionMode::Reduced
    } else {
        MotionMode::Full
    }
}

#[allow(dead_code)]
pub fn should_show_whisperer(phase_age: Duration, total_pre_stream: Duration) -> bool {
    phase_age >= PHASE_ANTI_FLICKER && total_pre_stream >= PRE_STREAM_ANTI_NOISE
}

pub fn phase_for_trace(trace: Option<&SystemTrace>) -> BackendPhase {
    match trace.and_then(|trace| trace.active_tool_index.and_then(|idx| trace.items.get(idx))) {
        Some(TraceItem::ToolCall(_)) | Some(TraceItem::ApprovalPrompt(_)) => {
            BackendPhase::ChoosingTools
        }
        Some(TraceItem::Thinking(_)) => BackendPhase::Thinking,
        None => BackendPhase::RecallingContext,
    }
}

pub fn likely_long_answer(prompt: &str, attachments: usize) -> bool {
    let words = prompt.split_whitespace().count();
    attachments > 0
        || words >= 40
        || prompt.contains('\n')
        || [
            "explain",
            "plan",
            "analyze",
            "compare",
            "summarize",
            "implement",
        ]
        .iter()
        .any(|needle| prompt.to_ascii_lowercase().contains(needle))
}

pub fn skeleton_widths(seed: &str) -> [f32; 3] {
    let hash = seed.bytes().fold(0x811c9dc5_u32, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ u32::from(byte)
    });
    let jitter = |shift: u32, span: f32| ((hash >> shift) & 0xf) as f32 / 15.0 * span;
    [
        0.96 + jitter(0, 0.04),
        0.76 + jitter(4, 0.08),
        0.50 + jitter(8, 0.08),
    ]
}

pub fn tool_trace_chip(trace: &SystemTrace) -> Option<String> {
    let tool_count = trace
        .items
        .iter()
        .filter(|item| matches!(item, TraceItem::ToolCall(_)))
        .count();
    if tool_count == 0 {
        return None;
    }

    let elapsed = trace
        .total_duration
        .or_else(|| {
            let total = trace.items.iter().filter_map(|item| match item {
                TraceItem::ToolCall(tool) => tool.duration,
                TraceItem::Thinking(thinking) => thinking.duration,
                TraceItem::ApprovalPrompt(_) => None,
            });
            total.reduce(|acc, next| acc + next)
        })
        .unwrap_or_default();
    let label = if tool_count == 1 { "tool" } else { "tools" };
    Some(format!(
        "{tool_count} {label} · {:.1} s",
        elapsed.as_secs_f32()
    ))
}

pub fn has_running_trace(trace: Option<&SystemTrace>) -> bool {
    trace.is_some_and(|trace| {
        trace.active_tool_index.is_some()
            || trace.items.iter().any(|item| match item {
                TraceItem::ToolCall(tool) => matches!(tool.state, ToolCallState::Running),
                TraceItem::Thinking(thinking) => thinking.state.is_processing(),
                TraceItem::ApprovalPrompt(_) => false,
            })
    })
}

pub fn smoothing_chars_per_frame(buffer_len: usize) -> usize {
    (buffer_len / 4).clamp(1, 3)
}

#[allow(dead_code)]
pub fn smoothing_cps(buffer_len: usize) -> usize {
    if buffer_len > STREAM_SMOOTH_ACCELERATE_AFTER {
        STREAM_SMOOTH_MAX_CPS
    } else {
        STREAM_SMOOTH_BASELINE_CPS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::views::message_types::{SystemTrace, ToolCallBlock, ToolSource};

    #[test]
    fn whisperer_obeys_anti_flicker_and_anti_noise() {
        assert!(!should_show_whisperer(
            Duration::from_millis(249),
            Duration::from_millis(900)
        ));
        assert!(!should_show_whisperer(
            Duration::from_millis(300),
            Duration::from_millis(599)
        ));
        assert!(should_show_whisperer(
            Duration::from_millis(250),
            Duration::from_millis(600)
        ));
    }

    #[test]
    fn phase_copy_is_non_negotiable() {
        assert_eq!(BackendPhase::RecallingContext.copy(), "Recalling context");
        assert_eq!(BackendPhase::ChoosingTools.copy(), "Choosing tools");
        assert_eq!(BackendPhase::Thinking.copy(), "Thinking");
    }

    #[test]
    fn skeleton_widths_stay_in_brief_ranges() {
        let widths = skeleton_widths("seed");
        assert!((0.96..=1.0).contains(&widths[0]));
        assert!((0.76..=0.84).contains(&widths[1]));
        assert!((0.50..=0.58).contains(&widths[2]));
    }

    #[test]
    fn trace_chip_uses_single_trace_source() {
        let mut trace = SystemTrace::new();
        trace.add_tool_call(ToolCallBlock {
            id: "1".to_string(),
            tool_name: "fetch".to_string(),
            display_name: "Fetch".to_string(),
            input: String::new(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: Some(Duration::from_millis(1200)),
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        });
        assert_eq!(tool_trace_chip(&trace).as_deref(), Some("1 tool · 1.2 s"));
    }

    #[test]
    fn stream_smoothing_limits_are_encoded() {
        assert_eq!(smoothing_chars_per_frame(0), 1);
        assert_eq!(smoothing_chars_per_frame(12), 3);
        assert_eq!(smoothing_cps(80), 30);
        assert_eq!(smoothing_cps(81), 90);
    }
}
