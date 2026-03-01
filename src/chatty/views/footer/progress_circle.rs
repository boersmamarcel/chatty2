use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ElementId, Hsla,
    InteractiveElement as _, IntoElement, ParentElement, Pixels, RenderOnce, StyleRefinement,
    Styled, Window, canvas, div, px, relative,
};
use gpui_component::plot::shape::{Arc, ArcData};
use gpui_component::{ActiveTheme as _, PixelsExt as _, Sizable, Size, StyledExt as _};
use std::f32::consts::TAU;
use std::time::Duration;

struct ProgressCircleState {
    value: f32,
}

/// A circular progress indicator element.
///
/// Ported from the upstream gpui-component ProgressCircle (not yet released in 0.5.1).
#[derive(IntoElement)]
pub struct ProgressCircle {
    id: ElementId,
    style: StyleRefinement,
    color: Option<Hsla>,
    value: f32,
    size: Size,
    children: Vec<AnyElement>,
}

impl ProgressCircle {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: Default::default(),
            color: None,
            style: StyleRefinement::default(),
            size: Size::default(),
            children: Vec::new(),
        }
    }

    pub fn value(mut self, value: f32) -> Self {
        self.value = value.clamp(0., 100.);
        self
    }

    fn render_circle(current_value: f32, color: Hsla) -> impl IntoElement {
        struct PrepaintState {
            current_value: f32,
            actual_inner_radius: f32,
            actual_outer_radius: f32,
            bounds: Bounds<Pixels>,
        }

        canvas(
            {
                let display_value = current_value;
                move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
                    let stroke_width = (bounds.size.width * 0.15).min(px(5.));
                    let actual_size = bounds.size.width.min(bounds.size.height);
                    let actual_radius = (actual_size.as_f32() - stroke_width.as_f32()) / 2.;
                    let actual_inner_radius = actual_radius - stroke_width.as_f32() / 2.;
                    let actual_outer_radius = actual_radius + stroke_width.as_f32() / 2.;

                    PrepaintState {
                        current_value: display_value,
                        actual_inner_radius,
                        actual_outer_radius,
                        bounds,
                    }
                }
            },
            move |_bounds, prepaint, window: &mut Window, _cx: &mut App| {
                // Background circle
                let bg_arc_data = ArcData {
                    data: &(),
                    index: 0,
                    value: 100.,
                    start_angle: 0.,
                    end_angle: TAU,
                    pad_angle: 0.,
                };

                let bg_arc = Arc::new()
                    .inner_radius(prepaint.actual_inner_radius)
                    .outer_radius(prepaint.actual_outer_radius);

                bg_arc.paint(
                    &bg_arc_data,
                    color.opacity(0.2),
                    None,
                    None,
                    &prepaint.bounds,
                    window,
                );

                // Progress arc
                if prepaint.current_value > 0. {
                    let progress_angle = (prepaint.current_value / 100.) * TAU;
                    let progress_arc_data = ArcData {
                        data: &(),
                        index: 1,
                        value: prepaint.current_value,
                        start_angle: 0.,
                        end_angle: progress_angle,
                        pad_angle: 0.,
                    };

                    let progress_arc = Arc::new()
                        .inner_radius(prepaint.actual_inner_radius)
                        .outer_radius(prepaint.actual_outer_radius);

                    progress_arc.paint(
                        &progress_arc_data,
                        color,
                        None,
                        None,
                        &prepaint.bounds,
                        window,
                    );
                }
            },
        )
        .absolute()
        .size_full()
    }
}

impl Styled for ProgressCircle {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Sizable for ProgressCircle {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl ParentElement for ProgressCircle {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ProgressCircle {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let value = self.value;
        let state =
            window.use_keyed_state(self.id.clone(), cx, |_, _| ProgressCircleState { value });
        let prev_value = state.read(cx).value;

        // Persist new value so the next render has the correct baseline
        let has_changed = (prev_value - value).abs() > 0.01;
        if has_changed {
            state.update(cx, |s, _| s.value = value);
        }

        let color = self.color.unwrap_or(cx.theme().progress_bar);

        div()
            .id(self.id.clone())
            .flex()
            .items_center()
            .justify_center()
            .line_height(relative(1.))
            .map(|this| match self.size {
                Size::XSmall => this.size_2(),
                Size::Small => this.size_3(),
                Size::Medium => this.size_4(),
                Size::Large => this.size_5(),
                Size::Size(s) => this.size(s * 0.75),
            })
            .refine_style(&self.style)
            .children(self.children)
            .map(|this| {
                if has_changed {
                    this.with_animation(
                        ElementId::Name(format!("progress-circle-{}", prev_value).into()),
                        Animation::new(Duration::from_secs_f64(0.15)),
                        move |this, delta| {
                            let animated_value = prev_value + (value - prev_value) * delta;
                            this.child(Self::render_circle(animated_value, color))
                        },
                    )
                    .into_any_element()
                } else {
                    this.child(Self::render_circle(value, color))
                        .into_any_element()
                }
            })
    }
}
