//! The system tray menu: its items, their labels and what each one does.
//!
//! The labels are read from the settings snapshot rather than translated here, so
//! the menu and the settings window cannot disagree about the product's language.

use super::*;

pub(crate) fn system_menu_presentation(snapshot: &SettingsSnapshot) -> SystemMenuPresentation {
    // `catalog_locale` is the protocol's own answer to "which catalog does this
    // resolved language read from". Restating that mapping here is what left the
    // tray menu in English for every language other than Chinese.
    let locale = snapshot.resolved_language.catalog_locale();
    let text = |key| bongocat_i18n::text(locale, key).to_owned();
    let product = text("system_menu.title");
    SystemMenuPresentation {
        title: product.clone(),
        // The tooltip is a creation-time input that cannot be changed later
        // (see `NativeSystemMenu`), which is exactly right for a version: it is
        // fixed for the life of the process. Hovering the tray icon is the
        // fastest way to answer "which build am I running".
        tooltip: format!("{product} {}", bongocat_app::PRODUCT_VERSION),
        open_settings: text("system_menu.open_settings"),
        model_window: text("navigation.model_window.title"),
        hide_overlay: text("settings.overlay.hide_model_window.label"),
        click_through: text("settings.overlay.click_through.label"),
        always_on_top: text("settings.overlay.always_on_top.label"),
        hide_on_pointer_hover: text("settings.overlay.hide_on_mouse_hover.label"),
        check_for_updates: text("update.about.label"),
        quit: text("system_menu.quit"),
        overlay_visible: snapshot.overlay_visible,
        click_through_enabled: snapshot.overlay.click_through,
        always_on_top_enabled: snapshot.overlay.always_on_top,
        hide_on_pointer_hover_enabled: snapshot.overlay.hide_on_pointer_hover,
        // Never shown. This product has no update feed: a release is a zip the
        // user replaces by hand, so the row could only ever answer "no update".
        // Leaving it out also keeps the tray menu from looking like it has a
        // feature the build cannot perform.
        update_check_available: false,
    }
}

pub(crate) async fn refresh_system_menu_presentation(
    client: &SettingsClient,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let snapshot = client
        .read_snapshot()
        .await
        .map_err(|error| error.to_string())?;
    let presentation = system_menu_presentation(&snapshot);
    cx.update(|cx| {
        if !cx.has_global::<ProductCoordinator>() {
            return Err("system menu owner is unavailable".to_owned());
        }
        cx.global_mut::<ProductCoordinator>()
            .system_menu
            .as_mut()
            .ok_or_else(|| "system menu owner is unavailable".to_owned())?
            .set_presentation(presentation)
            .map_err(|error| error.to_string())
    })
}

pub(crate) async fn apply_system_menu_overlay_action(
    client: SettingsClient,
    action: SystemMenuAction,
) -> Result<bool, String> {
    let snapshot = client
        .read_snapshot()
        .await
        .map_err(|error| error.to_string())?;
    let revision = snapshot
        .config_revision
        .ok_or_else(|| "system menu cannot update configuration during recovery".to_owned())?;
    match action {
        SystemMenuAction::ToggleOverlayVisibility => {
            client
                .set_overlay_visible(revision, !snapshot.overlay_visible)
                .await
        }
        SystemMenuAction::ToggleClickThrough => {
            client
                .set_overlay_settings(
                    revision,
                    SettingsOverlay {
                        click_through: !snapshot.overlay.click_through,
                        ..snapshot.overlay
                    },
                )
                .await
        }
        SystemMenuAction::ToggleAlwaysOnTop => {
            client
                .set_overlay_settings(
                    revision,
                    SettingsOverlay {
                        always_on_top: !snapshot.overlay.always_on_top,
                        ..snapshot.overlay
                    },
                )
                .await
        }
        SystemMenuAction::ToggleHideOnPointerHover => {
            client
                .set_overlay_settings(
                    revision,
                    SettingsOverlay {
                        hide_on_pointer_hover: !snapshot.overlay.hide_on_pointer_hover,
                        ..snapshot.overlay
                    },
                )
                .await
        }
        _ => return Err("invalid system menu overlay action".to_owned()),
    }
    .map(|_| true)
    .map_err(|error| error.to_string())
}
