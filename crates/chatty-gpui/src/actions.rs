//! Action registration and (on macOS) the menu bar.
//!
//! `register_actions` wires GPUI actions (New chat, Save, Quit, etc.) to
//! their handlers via `with_chatty_app`. `set_app_menus` constructs the
//! native macOS menu bar that triggers those same actions.

use super::*;

pub(crate) fn register_actions(cx: &mut App) {
    // Register open settings action with platform-specific keybindings
    debug!("Action registered");

    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-n", NewConversation, None),
        KeyBinding::new("cmd-up", PreviousConversation, None),
        KeyBinding::new("cmd-down", NextConversation, None),
        KeyBinding::new("alt-backspace", DeleteActiveConversation, None),
        KeyBinding::new("cmd-backspace", DeleteActiveConversation, None),
    ]);

    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-n", NewConversation, None),
        KeyBinding::new("ctrl-up", PreviousConversation, None),
        KeyBinding::new("ctrl-down", NextConversation, None),
        KeyBinding::new("ctrl-backspace", DeleteActiveConversation, None),
    ]);
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        debug!("Action triggered");
        SettingsView::open_or_focus_settings_window(cx);
    });
    cx.on_action(|_: &Quit, cx: &mut App| {
        debug!("Quit action triggered");
        chatty::services::cleanup_thumbnails();

        // Stop all active streams gracefully
        if let Some(manager) = cx
            .try_global::<chatty::models::GlobalStreamManager>()
            .and_then(|g| g.get())
        {
            manager.update(cx, |mgr, cx| {
                mgr.stop_all(cx);
            });
        }

        // Disconnect from all MCP servers on quit.
        let mcp_service = cx.global::<chatty::services::McpService>().clone();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            if let Err(e) = mcp_service.disconnect_all().await {
                error!(error = ?e, "Failed to disconnect MCP servers during shutdown");
            }
        })
        .detach();

        // Mandatory auto-update: if an update has been downloaded and is ready,
        // install it silently before quitting so the next launch runs the new
        // version. install_on_quit() does NOT relaunch — the user intended to
        // quit, so we respect that while still ensuring the update is applied.
        if let Some(updater) = cx.try_global::<AutoUpdater>()
            && matches!(updater.status(), auto_updater::AutoUpdateStatus::Ready(..))
        {
            info!("Pending update found on quit — installing before exit");
            cx.update_global::<AutoUpdater, _>(|updater, cx| {
                updater.install_on_quit(cx);
            });
            return;
        }

        cx.quit();
    });
    cx.on_action(|_: &ToggleSidebar, cx: &mut App| {
        debug!("Toggle sidebar action triggered");
        with_chatty_app(cx, |app, cx| {
            app.sidebar_view.update(cx, |sidebar, cx| {
                sidebar.toggle_collapsed(cx);
            });
        });
    });
    cx.on_action(|_: &NewConversation, cx: &mut App| {
        debug!("New conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.start_new_conversation(cx);
        });
    });
    cx.on_action(|_: &PreviousConversation, cx: &mut App| {
        debug!("Previous conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.navigate_conversation(-1, cx);
        });
    });
    cx.on_action(|_: &NextConversation, cx: &mut App| {
        debug!("Next conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.navigate_conversation(1, cx);
        });
    });
    cx.on_action(|_: &DeleteActiveConversation, cx: &mut App| {
        debug!("Delete active conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.delete_active_conversation(cx);
        });
    });
    cx.on_action(|_: &InstallCli, cx: &mut App| {
        debug!("Install CLI action triggered");
        cli_installer::install_cli(cx);
    });
}

#[cfg(target_os = "macos")]
pub(crate) fn set_app_menus(cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: "Chatty".into(),
        items: vec![
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Settings", OpenSettings),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::separator(),
            MenuItem::action("Install CLI\u{2026}", InstallCli),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ],
    }]);
}

