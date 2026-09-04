//! Shared provider-credential field.
//!
//! The Providers settings page it was written for is gone — provider
//! credentials now live in the Models & Providers roster's "Manage keys"
//! sheet (`views::models_page::provider_keys_sheet`). This field is still
//! used by other settings pages that hold an API key of their own.

use gpui::{App, Entity, SharedString, Styled, Window, prelude::FluentBuilder as _};
use gpui_component::{
    AxisExt as _,
    input::{Input, InputEvent, InputState},
    setting::{RenderOptions, SettingField},
};
use std::rc::Rc;

/// Create a masked API key input field with an eye toggle for visibility.
pub fn masked_api_key_field<V, S>(value: V, set_value: S) -> SettingField<SharedString>
where
    V: Fn(&App) -> SharedString + 'static,
    S: Fn(SharedString, &mut App) + 'static,
{
    type SetValueFn = dyn Fn(SharedString, &mut App);
    let set_value: Rc<SetValueFn> = Rc::new(set_value);

    SettingField::render(
        move |options: &RenderOptions, window: &mut Window, cx: &mut App| {
            let current_value = (value)(cx);
            let set_value = set_value.clone();

            struct MaskedInputState {
                input: Entity<InputState>,
                _subscription: gpui::Subscription,
            }

            let state = window
                .use_keyed_state(
                    SharedString::from(format!(
                        "masked-api-key-{}-{}-{}",
                        options.page_ix, options.group_ix, options.item_ix
                    )),
                    cx,
                    |window, cx| {
                        let input = cx.new(|cx| {
                            InputState::new(window, cx)
                                .default_value(current_value)
                                .masked(true)
                        });
                        let set_value = set_value.clone();
                        let _subscription = cx.subscribe(&input, {
                            move |_, input: Entity<InputState>, event: &InputEvent, cx| {
                                if let InputEvent::Change = event {
                                    let val = input.read(cx).value();
                                    (set_value)(val, cx);
                                }
                            }
                        });
                        MaskedInputState {
                            input,
                            _subscription,
                        }
                    },
                )
                .read(cx);

            Input::new(&state.input)
                .mask_toggle()
                .with_size(options.size)
                .map(|this| {
                    if options.layout.is_horizontal() {
                        this.w_64()
                    } else {
                        this.w_full()
                    }
                })
        },
    )
}
