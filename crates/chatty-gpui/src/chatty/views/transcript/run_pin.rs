use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, IconName, Sizable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPinKind {
    JumpToLatest,
    PendingApproval,
}

type RunPinClick = Box<dyn Fn(&mut App) + 'static>;

#[derive(IntoElement)]
pub struct RunPin {
    kind: RunPinKind,
    visible: bool,
    on_click: Option<RunPinClick>,
}

impl RunPin {
    pub fn new(kind: RunPinKind) -> Self {
        Self {
            kind,
            visible: true,
            on_click: None,
        }
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for RunPin {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if !self.visible {
            return div();
        }
        let (id, label, icon) = match self.kind {
            RunPinKind::JumpToLatest => ("run-pin-latest", "Jump to latest", IconName::ArrowDown),
            RunPinKind::PendingApproval => (
                "run-pin-approval",
                "Approval needed",
                IconName::TriangleAlert,
            ),
        };
        let on_click = self.on_click;
        div().absolute().bottom_4().right_4().child(
            Button::new(id)
                .primary()
                .small()
                .icon(Icon::new(icon).size_3())
                .label(label)
                .on_click(move |_, _, cx| {
                    if let Some(cb) = &on_click {
                        cb(cx);
                    }
                }),
        )
    }
}
