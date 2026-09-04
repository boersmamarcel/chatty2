//! Transcript scroll policy: when to follow the stream, when to show the
//! "Jump to latest" pin.
//!
//! Kept as pure functions over a measured distance so the policy can be tested
//! without a window. The rendering side supplies the measurement and applies
//! the decision (AGE-180).

use gpui::{Bounds, Pixels, px};

/// How close to the bottom still counts as "at the bottom" for sticky scroll.
///
/// Deliberately loose. Layout settling — an image loading, a math SVG
/// rendering, a code block expanding, a streamed turn re-measuring — moves the
/// content by more than a few pixels without the user touching anything. The
/// old 10px threshold treated every one of those as a deliberate scroll-away
/// and latched the pin on for the rest of the conversation.
pub(super) const STICKY_BOTTOM_EPSILON: Pixels = px(48.0);

/// How far above the bottom the user must be before the pin appears.
///
/// Roughly a screenful of content: the pin is for "you have scrolled away",
/// not for "the layout moved".
pub(super) const PIN_SHOW_DISTANCE: Pixels = px(300.0);

/// How close to the bottom the user must return before the pin disappears.
///
/// Strictly less than [`PIN_SHOW_DISTANCE`]: with one threshold the pin
/// flickers on and off while scrolling around the boundary.
pub(super) const PIN_HIDE_DISTANCE: Pixels = px(120.0);

/// Clearance the plan card must have above the viewport before the sticky
/// strip replaces it. Without it the two swap back and forth on the boundary.
const PLAN_STRIP_CLEARANCE: Pixels = px(8.0);

/// True when the inline plan card has fully scrolled above the viewport.
///
/// `bounds` is `ListState::bounds_for_item(plan_ix)`, which is `None` in two
/// different situations — the item starts above the list's scroll anchor, or it
/// has never been measured. `anchor_ix` (`logical_scroll_top().item_ix`) tells
/// them apart: an item before the anchor is above the viewport, an item after
/// it that has not been measured is below.
pub(super) fn plan_is_above_viewport(
    bounds: Option<Bounds<Pixels>>,
    plan_ix: usize,
    anchor_ix: usize,
    viewport_top: Pixels,
) -> bool {
    match bounds {
        None => plan_ix < anchor_ix,
        Some(bounds) => bounds.bottom() + PLAN_STRIP_CLEARANCE <= viewport_top,
    }
}

/// What the transcript should do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScrollDecision {
    /// Keep following the bottom as content grows.
    pub stick: bool,
    /// Show the "Jump to latest" pin.
    pub show_pin: bool,
}

/// Resolve the scroll policy from the current measurement.
///
/// `distance_from_bottom` is `max_offset.height + offset.y`: zero at the
/// bottom, growing as the user scrolls up. `measured` is false before the list
/// has laid out (max offset still zero), when nothing can be concluded and the
/// previous state must be preserved rather than guessed at.
pub(super) fn resolve_scroll_state(
    distance_from_bottom: Pixels,
    measured: bool,
    was_sticky: bool,
    pin_was_visible: bool,
) -> ScrollDecision {
    if !measured {
        // Nothing to measure yet: a short conversation that fits on screen has
        // no scroll range, and it has certainly not been scrolled away from.
        return ScrollDecision {
            stick: was_sticky,
            show_pin: false,
        };
    }

    let at_bottom = distance_from_bottom <= STICKY_BOTTOM_EPSILON;

    // Hysteresis: cross PIN_SHOW_DISTANCE to appear, fall back under
    // PIN_HIDE_DISTANCE to disappear, hold state in between.
    let show_pin = if distance_from_bottom >= PIN_SHOW_DISTANCE {
        true
    } else if distance_from_bottom <= PIN_HIDE_DISTANCE {
        false
    } else {
        pin_was_visible
    };

    // Following resumes on its own once the user is back at the bottom, and
    // stops the moment they scroll meaningfully away.
    let stick = if at_bottom {
        true
    } else {
        was_sticky && !show_pin
    };

    ScrollDecision { stick, show_pin }
}

#[cfg(test)]
mod plan_strip_tests {
    use super::*;
    use gpui::{Bounds, point, size};

    fn bounds_at(top: f32, height: f32) -> Option<Bounds<Pixels>> {
        Some(Bounds {
            origin: point(px(0.0), px(top)),
            size: size(px(400.0), px(height)),
        })
    }

    #[test]
    fn straddling_the_top_edge_keeps_the_strip_hidden() {
        // Card runs 80..200, viewport starts at 100: still partly on screen.
        assert!(!plan_is_above_viewport(
            bounds_at(80.0, 120.0),
            3,
            0,
            px(100.0)
        ));
    }

    #[test]
    fn clearance_keeps_the_boundary_quiet() {
        // Bottom at 120, viewport top at 126: past, but inside the clearance.
        assert!(!plan_is_above_viewport(
            bounds_at(20.0, 100.0),
            3,
            0,
            px(126.0)
        ));
    }

    #[test]
    fn fully_past_the_top_shows_the_strip() {
        assert!(plan_is_above_viewport(
            bounds_at(20.0, 100.0),
            3,
            0,
            px(130.0)
        ));
    }

    /// `bounds_for_item` returns `None` both for an item above the list's
    /// anchor and for one it has never measured. Only the first is "above".
    #[test]
    fn unmeasured_above_the_anchor_counts_as_scrolled_past() {
        assert!(plan_is_above_viewport(None, 2, 5, px(0.0)));
    }

    #[test]
    fn unmeasured_below_the_anchor_does_not() {
        assert!(!plan_is_above_viewport(None, 7, 5, px(0.0)));
    }

    #[test]
    fn the_anchor_item_itself_is_not_above() {
        assert!(!plan_is_above_viewport(None, 5, 5, px(0.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported case: a single-turn conversation with everything on
    /// screen. Nothing is scrolled away, so no pin.
    #[test]
    fn unscrollable_conversation_shows_no_pin() {
        let d = resolve_scroll_state(px(0.0), false, true, false);
        assert!(!d.show_pin);
        assert!(d.stick);
    }

    /// A previously-visible pin must not survive into a conversation with no
    /// scroll range (switching to a short chat).
    #[test]
    fn unmeasured_list_hides_a_stale_pin() {
        let d = resolve_scroll_state(px(0.0), false, false, true);
        assert!(!d.show_pin);
    }

    /// Layout settling moves the content a little. That is not a scroll-away,
    /// and it must not latch the pin on — the old 10px threshold did.
    #[test]
    fn layout_settling_does_not_show_the_pin() {
        for jitter in [1.0, 10.0, 40.0, 47.0] {
            let d = resolve_scroll_state(px(jitter), true, true, false);
            assert!(!d.show_pin, "{jitter}px of settling showed the pin");
            assert!(d.stick, "{jitter}px of settling stopped sticky scroll");
        }
    }

    #[test]
    fn scrolling_a_screenful_away_shows_the_pin_and_stops_following() {
        let d = resolve_scroll_state(px(600.0), true, true, false);
        assert!(d.show_pin);
        assert!(!d.stick);
    }

    /// The half of the bug that made the pin permanent: nothing cleared it on
    /// the way back down.
    #[test]
    fn scrolling_back_to_the_bottom_hides_the_pin_again() {
        let away = resolve_scroll_state(px(600.0), true, true, false);
        assert!(away.show_pin);

        let back = resolve_scroll_state(px(20.0), true, away.stick, away.show_pin);
        assert!(!back.show_pin, "the pin must clear when the user returns");
        assert!(back.stick, "following must resume at the bottom");
    }

    #[test]
    fn pin_holds_state_between_the_two_thresholds() {
        // Coming from hidden, mid-band stays hidden.
        let d = resolve_scroll_state(px(200.0), true, true, false);
        assert!(!d.show_pin);
        // Coming from visible, mid-band stays visible — no flicker.
        let d = resolve_scroll_state(px(200.0), true, false, true);
        assert!(d.show_pin);
    }

    #[test]
    fn thresholds_are_ordered_for_hysteresis() {
        assert!(PIN_HIDE_DISTANCE < PIN_SHOW_DISTANCE);
        assert!(STICKY_BOTTOM_EPSILON <= PIN_HIDE_DISTANCE);
    }
}
