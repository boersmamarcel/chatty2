use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::views::model_form_view::ModelFormView;
use gpui::{App, AppContext, AsyncApp, Bounds, Global, Point, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, px, size};
use gpui_component::Root;

// Global state to track the model form modal window handle
pub struct GlobalModelFormWindow {
    pub handle: Option<WindowHandle<Root>>,
}

impl Default for GlobalModelFormWindow {
    fn default() -> Self {
        Self { handle: None }
    }
}

impl Global for GlobalModelFormWindow {}

/// Open modal to create a new model
pub fn open_create_model_modal(cx: &mut App) {
    // Check if modal is already open
    if let Some(handle) = cx.global::<GlobalModelFormWindow>().handle.as_ref() {
        // Try to focus existing window
        let _ = handle.update(cx, |_view, window, _cx| {
            window.activate_window();
        });
        return;
    }

    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Add Model".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Point::default(),
            size: size(px(500.0), px(600.0)),
        })),
        window_min_size: Some(size(px(400.0), px(500.0))),
        ..Default::default()
    };

    if let Ok(window_handle) = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ModelFormView::new_create(window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    }) {
        cx.global_mut::<GlobalModelFormWindow>().handle = Some(window_handle);
    }
}

/// Open modal to edit an existing model
pub fn open_edit_model_modal(model_id: String, cx: &mut App) {
    // Get the model config
    let model = cx.global::<ModelsModel>().get_model(&model_id);
    let Some(model) = model else {
        eprintln!("Model not found: {}", model_id);
        return;
    };
    let model = model.clone();

    // Check if modal is already open
    if let Some(handle) = cx.global::<GlobalModelFormWindow>().handle.as_ref() {
        // Try to focus existing window
        let _ = handle.update(cx, |_view, window, _cx| {
            window.activate_window();
        });
        return;
    }

    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Edit Model".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Point::default(),
            size: size(px(500.0), px(600.0)),
        })),
        window_min_size: Some(size(px(400.0), px(500.0))),
        ..Default::default()
    };

    if let Ok(window_handle) = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ModelFormView::new_edit(&model, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    }) {
        cx.global_mut::<GlobalModelFormWindow>().handle = Some(window_handle);
    }
}

/// Create a new model
pub fn create_model(config: ModelConfig, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<ModelsModel>();
    model.add_model(config);

    // 2. Get updated state for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_models_async(models_to_save, cx);
}

/// Update an existing model
pub fn update_model(updated_config: ModelConfig, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<ModelsModel>();

    if !model.update_model(updated_config) {
        eprintln!("Failed to update model: model not found");
        return;
    }

    // 2. Get updated state for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_models_async(models_to_save, cx);
}

/// Delete a model by ID
pub fn delete_model(model_id: String, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<ModelsModel>();

    if !model.delete_model(&model_id) {
        eprintln!("Failed to delete model: model not found");
        return;
    }

    // 2. Get updated state for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_models_async(models_to_save, cx);
}

/// Save models asynchronously to disk
fn save_models_async(models: Vec<ModelConfig>, cx: &mut App) {
    use crate::MODELS_REPOSITORY;

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = MODELS_REPOSITORY.clone();
        if let Err(e) = repo.save_all(models).await {
            eprintln!("Failed to save models: {}", e);
            eprintln!("Changes will be lost on restart - please try again");
        }
    })
    .detach();
}
