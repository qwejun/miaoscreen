use super::*;
use gpui_kit::{FileDropEvent, canvas};

fn with_search_keywords<I>(items: I, keywords: &[SharedString]) -> impl Iterator<Item = SettingItem>
where
    I: IntoIterator<Item = SettingItem>,
{
    items
        .into_iter()
        .map(move |item| item.keywords(keywords.iter().cloned()))
}

/// One model dropdown of the gamepad auto switch.
///
/// The row is a custom element because the option list is the model catalog, so
/// it cannot be a static field. The gate still reaches the control itself: the
/// component forwards the row's disabled state to the renderer, and the select
/// reads it instead of recomputing the condition. The options are passed in
/// whole rather than as labels, so the menu width is measured from the very list
/// the dropdown shows.
fn gamepad_auto_switch_model_row(
    label_key: &'static str,
    language: SettingsLanguage,
    choices: Vec<GamepadModelChoice>,
    select: &Entity<GamepadModelSelectState>,
    gate: SettingGate,
) -> SettingItem {
    let select = select.clone();
    SettingItem::new(
        bongocat_i18n::text(language.catalog_locale(), label_key),
        SettingField::element(
            move |options: &RenderOptions, window: &mut Window, _: &mut App| {
                Select::new(&select)
                    .menu_width(dropdown_menu_width(
                        window,
                        choices.iter().map(|choice| choice.title()),
                    ))
                    .disabled(options.is_disabled())
                    .into_any_element()
            },
        ),
    )
    .disabled(gate.disables_controls())
}

/// `gpui-kit` owns the sidebar selection in window-keyed state. The empty title
/// suffix is the component's public per-page render hook, so it lets the process
/// owner observe the active page without taking over the component's navigation.
fn report_navigation_page(page: SettingsNavigationPage, memory: &SettingsNavigationMemory) {
    memory.select(page);
}

fn page_reporter(
    page: SettingsNavigationPage,
    memory: SettingsNavigationMemory,
) -> impl Fn(&mut Window, &mut App) -> Div {
    move |_, _| {
        report_navigation_page(page, &memory);
        div()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        if viewport.width <= px(0.) || viewport.height <= px(0.) {
            return div().size_full().into_any_element();
        }
        // `on_file_drop_exit` is intentionally hitbox-scoped in GPUI. Once the
        // pointer is outside the window, that hitbox is no longer hovered and
        // the element callback can miss the platform's DragLeave/DragExit event.
        // Register a window-level listener from a paint-phase canvas instead;
        // it remains active even when the drag leaves from outside the viewport.
        let drag_exit_view = cx.entity().downgrade();
        let model_drag_exit_listener = canvas(|_bounds, _window, _cx| {}, {
            let drag_exit_view = drag_exit_view.clone();
            move |_bounds, _state, window, _cx| {
                window.on_mouse_event(move |event: &FileDropEvent, _phase, _window, cx| {
                    if matches!(event, FileDropEvent::Exited) {
                        let _ = drag_exit_view.update(cx, |view, cx| view.clear_model_drag(cx));
                    }
                });
            }
        })
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let snapshot = self.snapshot.clone();
        // The appearance is applied on every frame, seeded included: the first frame is
        // painted before the service answers the first snapshot, and it has to be the
        // product's theme rather than the component default.
        let appearance_theme = self.display_appearance_theme();
        self.sync_component_theme(appearance_theme, window, cx);
        if let Some(snapshot) = snapshot.as_ref() {
            self.sync_component_inputs(snapshot, window, cx);
        }
        // Every page renders its rows from the stable states where editing is
        // structurally impossible — no snapshot, unusable configuration, a
        // model import running. The transient in-flight `pending` flag must
        // not feed any gate or row: it flips on and off around every save and
        // visibly dims and re-enables the page on each control change, which
        // reads as the page refreshing. `editing_blocked` is that one
        // predicate, shared by every page's gate — the Appearance page's selects
        // and the startup switch below read it exactly like the gated rows do
        // (see `setting_gate` for the unified rule).
        let editing_blocked = self.editing_blocked(snapshot.as_ref());
        // The mode dropdown owns the gate, so it stays operable while the mode is
        // off — otherwise nothing could turn the behavior back on — and the
        // interval below it follows the dropdown's own state.
        let random_behavior_gate = SettingGate::new(
            editing_blocked,
            snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.random_behavior.mode.is_active()),
        );
        // The gamepad group closes with the switch that owns the two model
        // dropdowns below it: the same gate rule the hover-hide delay and the
        // shortcut scopes use.
        let gamepad_auto_switch_gate = SettingGate::new(
            editing_blocked,
            snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.gamepad_auto_switch.enabled),
        );
        // One gate per shortcut scope: the switch that owns the group plus the
        // shared structural editing state. Each scope's rows read their own
        // gate; the gate switches themselves stay operable while off.
        let command_shortcuts_gate = SettingGate::new(
            editing_blocked,
            snapshot
                .as_ref()
                .is_some_and(|snapshot| shortcuts_page::ShortcutScope::Window.is_enabled(snapshot)),
        );
        let behavior_shortcuts_gate = SettingGate::new(
            editing_blocked,
            snapshot
                .as_ref()
                .is_some_and(|snapshot| shortcuts_page::ShortcutScope::Model.is_enabled(snapshot)),
        );
        let shortcuts = snapshot
            .as_ref()
            .map(|snapshot| snapshot.shortcuts.clone())
            .unwrap_or_default();
        let active_model = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.active_model.as_ref());
        let model_entries = snapshot
            .as_ref()
            .map(|snapshot| snapshot.model_catalog.entries.as_slice())
            .unwrap_or_default();
        let language = self.display_language();
        // The two model dropdowns are view state, not render state, so the page
        // reads the very entities the view syncs and subscribes to.
        let gamepad_connected_model_select = self.gamepad_connected_model_select.clone();
        let gamepad_disconnected_model_select = self.gamepad_disconnected_model_select.clone();
        self.sync_model_selection_dialog(window, cx);
        self.sync_mver_mode_dialog(window, cx);
        self.sync_plugin_settings_dialog(window, cx);
        let plugin_cards = self.plugin_host.snapshot().cards;
        while let Some(report) = self.plugin_host.take_failure() {
            window.push_notification(
                Notification::new()
                    .id1::<PluginFailureNotification>(plugin_failure_notification_id(
                        &report.failure,
                    ))
                    .message(plugin_failure_message(language, &report, &plugin_cards))
                    .with_type(NotificationType::Error),
                cx,
            );
        }
        if self.plugin_host.take_update_success() {
            window.push_notification(
                Notification::new()
                    .id1::<PluginFailureNotification>("plugin-update-all")
                    .message(bongocat_i18n::text(
                        language.catalog_locale(),
                        "plugins.update_all_success",
                    ))
                    .with_type(NotificationType::Success),
                cx,
            );
        }
        if let Some(error) = self.pending_notification.take() {
            window.push_notification(
                Notification::new()
                    .id::<SettingsServiceErrorNotification>()
                    .message(settings_error(language, error))
                    .with_type(NotificationType::Error),
                cx,
            );
        }
        // Reported before the success below, so a run that installed several
        // models and could not prepare one of them reads as "this one failed"
        // first and "the rest are in" second.
        if self.model_import_failed_pending {
            self.model_import_failed_pending = false;
            window.push_notification(
                Notification::new()
                    .id::<ModelImportFailedNotification>()
                    .message(bongocat_i18n::text(
                        language.catalog_locale(),
                        "models.import.failed",
                    ))
                    .with_type(NotificationType::Error),
                cx,
            );
        }
        if self.model_import_success_pending {
            self.model_import_success_pending = false;
            window.push_notification(
                Notification::new()
                    .id::<ModelImportSuccessNotification>()
                    .message(bongocat_i18n::text(
                        language.catalog_locale(),
                        "models.import.success",
                    ))
                    .with_type(NotificationType::Success),
                cx,
            );
        }
        if self.about_copy_success_pending {
            self.about_copy_success_pending = false;
            window.push_notification(
                Notification::new()
                    .id::<AboutCopySuccessNotification>()
                    .message(bongocat_i18n::text(
                        language.catalog_locale(),
                        "about.software_information.copy_success",
                    ))
                    .with_type(NotificationType::Success),
                cx,
            );
        }
        self.sync_shortcut_row_focus(
            &shortcuts,
            active_model,
            model_entries,
            &snapshot
                .as_ref()
                .map(|snapshot| snapshot.model_behavior_names.clone())
                .unwrap_or_default(),
            editing_blocked,
            cx,
        );
        let view_entity = cx.entity();
        let navigation_memory = self.navigation_memory.clone();
        let startup_item = startup_item_presentation(
            snapshot.as_ref().map(|snapshot| snapshot.startup_item),
            editing_blocked,
            language,
        );

        // Theme and language stay directly on the first page. Repeating an
        // "Appearance" group below an "Appearance & language" page title would
        // add a level without helping the user find either control.
        let appearance_keywords =
            SettingsNavigationPage::Appearance.search_keywords(language, std::iter::empty());
        let appearance_page = SettingPage::new(SettingsNavigationPage::Appearance.title(language))
            .icon(SettingsNavigationPage::Appearance.icon())
            .default_open(true)
            .title_suffix(page_reporter(
                SettingsNavigationPage::Appearance,
                navigation_memory.clone(),
            ))
            .group(SettingGroup::new().items(with_search_keywords(
                vec![
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.appearance.theme.label",
                        ),
                        SettingField::element({
                            let view = view_entity.clone();
                            move |_: &RenderOptions, window: &mut Window, app: &mut App| {
                                let state = view.read(app).theme_select.clone();
                                Select::new(&state)
                                    .menu_width(dropdown_menu_width(
                                        window,
                                        theme_options(language),
                                    ))
                                    .disabled(editing_blocked)
                                    .into_any_element()
                            }
                        }),
                    ),
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.appearance.language.label",
                        ),
                        SettingField::element({
                            let view = view_entity.clone();
                            move |_: &RenderOptions, window: &mut Window, app: &mut App| {
                                let state = view.read(app).language_select.clone();
                                Select::new(&state)
                                    .menu_width(dropdown_menu_width(
                                        window,
                                        SettingsLanguage::ALL.into_iter().map(|option| {
                                            crate::settings_language_display_name(option, language)
                                        }),
                                    ))
                                    .disabled(editing_blocked)
                                    .into_any_element()
                            }
                        }),
                    ),
                ],
                &appearance_keywords,
            )));

        // The two gamepad dropdowns are sized from the choices this frame shows,
        // which is the list `sync_component_inputs` fills them with.
        let gamepad_connected_choices = gamepad_auto_switch_choices(
            snapshot.as_ref(),
            GamepadConnectionState::Connected,
            language,
        );
        let gamepad_disconnected_choices = gamepad_auto_switch_choices(
            snapshot.as_ref(),
            GamepadConnectionState::Disconnected,
            language,
        );
        // The device page: the round panel this build exists for. The window
        // settings the upstream product exposed (click-through, opacity, desktop
        // placement) describe a surface this fork does not draw, so the page the
        // slot used to hold is gone and the panel's own configuration lives here.
        let device_keywords = SettingsNavigationPage::DeviceScreen
            .search_keywords(language, ["settings.device_screen.strip.title"]);
        let device_address = device_page::device_link(cx)
            .map(|link| link.address())
            .unwrap_or_default();
        // The snapshot stream costs the panel a full framebuffer read-back and a
        // JPEG encode, so it is not kept running: one picture is asked for when
        // this page becomes visible, and another whenever a strip setting changes
        // (see `apply_device_config`). Between those moments the page shows the
        // last one, because nothing on the panel moves on its own.
        let device_page_visible = navigation_memory.page_index()
            == SettingsNavigationPage::DeviceScreen.index();
        if device_page_visible != self.device_preview_page {
            self.device_preview_page = device_page_visible;
            if device_page_visible {
                self.request_device_preview(cx);
                // The state row is only meaningful while the page is on screen,
                // and reading it costs one atomic load a second.
                self.watch_device_status(cx);
            } else {
                self.stop_device_preview(cx);
                self.stop_device_status();
            }
        }

        let device_screen_page =
            SettingPage::new(SettingsNavigationPage::DeviceScreen.title(language))
                .icon(SettingsNavigationPage::DeviceScreen.icon())
                .title_suffix(page_reporter(
                    SettingsNavigationPage::DeviceScreen,
                    navigation_memory.clone(),
                ))
                .groups(vec![
                    SettingGroup::new()
                        .title(bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.device_screen.connection.title",
                        ))
                        .items(with_search_keywords(
                            vec![
                                SettingItem::new(
                                    bongocat_i18n::text(
                                        language.catalog_locale(),
                                        "settings.device_screen.link.label",
                                    ),
                                    SettingField::dropdown(
                                        vec![
                                            (
                                                SharedString::from(DeviceLinkMode::Wireless.id()),
                                                SharedString::from(bongocat_i18n::text(
                                                    language.catalog_locale(),
                                                    "settings.device_screen.link.wireless",
                                                )),
                                            ),
                                            (
                                                SharedString::from(DeviceLinkMode::Wired.id()),
                                                SharedString::from(bongocat_i18n::text(
                                                    language.catalog_locale(),
                                                    "settings.device_screen.link.wired",
                                                )),
                                            ),
                                        ],
                                        {
                                            move |app: &App| {
                                                SharedString::from(
                                                    device_page::device_link(app)
                                                        .map(|link| link.mode().id())
                                                        .unwrap_or(DeviceLinkMode::Wireless.id()),
                                                )
                                            }
                                        },
                                        {
                                            let view = view_entity.clone();
                                            move |value: SharedString, app: &mut App| {
                                                let mode = DeviceLinkMode::from_id(&value);
                                                view.update(app, |view, cx| {
                                                    view.set_device_link_mode(mode, cx);
                                                });
                                            }
                                        },
                                    ),
                                ),
                                SettingItem::new(
                                    bongocat_i18n::text(
                                        language.catalog_locale(),
                                        "settings.device_screen.state.label",
                                    ),
                                    SettingField::element({
                                        let view = view_entity.clone();
                                        move |_: &RenderOptions, _: &mut Window, app: &mut App| {
                                            let state = view.read(app).device_link_state;
                                            let locale = language.catalog_locale();
                                            let (text, color) = match state {
                                                DeviceLinkState::Connected => (
                                                    bongocat_i18n::text(
                                                        locale,
                                                        "settings.device_screen.state.connected",
                                                    ),
                                                    app.theme().success,
                                                ),
                                                DeviceLinkState::Waiting => (
                                                    bongocat_i18n::text(
                                                        locale,
                                                        "settings.device_screen.state.waiting",
                                                    ),
                                                    app.theme().muted_foreground,
                                                ),
                                                DeviceLinkState::Unavailable(problem) => {
                                                    let key = match problem {
                                                        DeviceLinkProblem::AdbMissing => {
                                                            "settings.device_screen.state.adb_missing"
                                                        }
                                                        DeviceLinkProblem::NoCable => {
                                                            "settings.device_screen.state.no_cable"
                                                        }
                                                        DeviceLinkProblem::TunnelFailed => {
                                                            "settings.device_screen.state.tunnel_failed"
                                                        }
                                                        DeviceLinkProblem::LaunchFailed => {
                                                            "settings.device_screen.state.launch_failed"
                                                        }
                                                    };
                                                    (
                                                        bongocat_i18n::text(locale, key),
                                                        app.theme().danger,
                                                    )
                                                }
                                            };
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(div().size(px(8.)).rounded_full().bg(color))
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(color)
                                                        .child(text),
                                                )
                                                .into_any_element()
                                        }
                                    }),
                                ),
                                SettingItem::new(
                                    bongocat_i18n::text(
                                        language.catalog_locale(),
                                        "settings.device_screen.address.label",
                                    ),
                                    SettingField::element({
                                        let address = device_address.clone();
                                        move |_: &RenderOptions, _: &mut Window, app: &mut App| {
                                            div()
                                                .font_family("monospace")
                                                .text_sm()
                                                .text_color(app.theme().muted_foreground)
                                                .child(address.clone())
                                                .into_any_element()
                                        }
                                    }),
                                ),
                            ],
                            &device_keywords,
                        )),
                    SettingGroup::new()
                        .title(bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.device_screen.strip.title",
                        ))
                        .items(vec![
                            SettingItem::new(
                                bongocat_i18n::text(
                                    language.catalog_locale(),
                                    "settings.device_screen.strip.enabled.label",
                                ),
                                SettingField::switch(
                                    {
                                        let view = view_entity.clone();
                                        move |app| view.read(app).device_config().enabled
                                    },
                                    {
                                        let view = view_entity.clone();
                                        move |value: bool, app: &mut App| {
                                            view.update(app, |view, cx| {
                                                view.update_device_config(
                                                    |config| config.enabled = value,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ),
                            ),
                            SettingItem::new(
                                bongocat_i18n::text(
                                    language.catalog_locale(),
                                    "settings.device_screen.band.label",
                                ),
                                SettingField::number_input(
                                    NumberFieldOptions {
                                        min: 0.0,
                                        max: 240.0,
                                        step: 8.0,
                                    },
                                    {
                                        let view = view_entity.clone();
                                        move |app| f64::from(view.read(app).device_config().band_px)
                                    },
                                    {
                                        let view = view_entity.clone();
                                        move |value: f64, app: &mut App| {
                                            let band = value.round().clamp(0.0, 240.0) as u32;
                                            view.update(app, |view, cx| {
                                                view.update_device_config(
                                                    |config| config.band_px = band,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ),
                            ),
                            SettingItem::new(
                                bongocat_i18n::text(
                                    language.catalog_locale(),
                                    "settings.device_screen.clock.label",
                                ),
                                SettingField::input(
                                    {
                                        let view = view_entity.clone();
                                        move |app| {
                                            SharedString::from(
                                                view.read(app).device_config().clock.clone(),
                                            )
                                        }
                                    },
                                    {
                                        let view = view_entity.clone();
                                        move |value: SharedString, app: &mut App| {
                                            let text = value.to_string();
                                            view.update(app, |view, cx| {
                                                view.update_device_config(
                                                    |config| config.clock = text,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ),
                            ),
                            SettingItem::new(
                                bongocat_i18n::text(
                                    language.catalog_locale(),
                                    "settings.device_screen.date.label",
                                ),
                                SettingField::input(
                                    {
                                        let view = view_entity.clone();
                                        move |app| {
                                            SharedString::from(
                                                view.read(app).device_config().date.clone(),
                                            )
                                        }
                                    },
                                    {
                                        let view = view_entity.clone();
                                        move |value: SharedString, app: &mut App| {
                                            let text = value.to_string();
                                            view.update(app, |view, cx| {
                                                view.update_device_config(
                                                    |config| config.date = text,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ),
                            ),
                            SettingItem::new(
                                bongocat_i18n::text(
                                    language.catalog_locale(),
                                    "settings.device_screen.banner.label",
                                ),
                                SettingField::input(
                                    {
                                        let view = view_entity.clone();
                                        move |app| {
                                            SharedString::from(
                                                view.read(app).device_config().banner.clone(),
                                            )
                                        }
                                    },
                                    {
                                        let view = view_entity.clone();
                                        move |value: SharedString, app: &mut App| {
                                            let text = value.to_string();
                                            view.update(app, |view, cx| {
                                                view.update_device_config(
                                                    |config| config.banner = text,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ),
                            ),
                        ]),
                    SettingGroup::new()
                        .title(bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.device_screen.preview.title",
                        ))
                        .items(vec![SettingItem::new(
                            bongocat_i18n::text(
                                language.catalog_locale(),
                                "settings.device_screen.preview.title",
                            ),
                            SettingField::element({
                                let view = view_entity.clone();
                                move |_: &RenderOptions, _: &mut Window, app: &mut App| {
                                    let snapshot = view.read(app).device_preview.clone();
                                    let size = px(320.0);
                                    // The snapshot is a JPEG of the panel's own
                                    // framebuffer, so it is drawn as a picture
                                    // rather than rebuilt from settings: what the
                                    // circle shows is what the hardware shows,
                                    // bezel and all.
                                    let disc = div()
                                        .flex_none()
                                        .size(size)
                                        .rounded_full()
                                        .overflow_hidden();
                                    match snapshot {
                                        Some(image) => disc
                                            .bg(app.theme().background)
                                            .child(
                                                // The radius is on the picture
                                                // itself: the panel is round, and
                                                // an ancestor clip does not reach
                                                // an image the way it reaches a
                                                // filled div.
                                                img(image)
                                                    .size_full()
                                                    .object_fit(ObjectFit::Cover)
                                                    .rounded_full(),
                                            )
                                            .into_any_element(),
                                        None => disc
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .bg(app.theme().secondary)
                                            .text_xs()
                                            .text_color(app.theme().muted_foreground)
                                            .child(bongocat_i18n::text(
                                                language.catalog_locale(),
                                                "settings.device_screen.preview.waiting",
                                            ))
                                            .into_any_element(),
                                    }
                                }
                            }),
                        )]),
                ]);

        // Model-wide display and automatic behavior belong with the model the
        // user is choosing. Behavior shortcut bindings remain on Shortcuts:
        // that page owns discrete command mappings and their two gates.
        let model_behavior_keywords =
            SettingsNavigationPage::ModelBehavior.search_keywords(language, std::iter::empty());
        let model_behavior_group = SettingGroup::new().items(with_search_keywords(
            vec![
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.mirror_model.label",
                    ),
                    SettingField::switch(
                        {
                            let view = view_entity.clone();
                            move |app| {
                                view.read(app)
                                    .snapshot
                                    .as_ref()
                                    .is_some_and(|s| s.model_settings.mirror)
                            }
                        },
                        {
                            let view = view_entity.clone();
                            move |value, app| {
                                view.update(app, |view, cx| {
                                    if let Some(s) = view.snapshot.as_ref() {
                                        let mut settings = s.model_settings;
                                        settings.mirror = value;
                                        view.set_model_settings(settings, cx);
                                    }
                                });
                            }
                        },
                    ),
                ),
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.motion_audio.label",
                    ),
                    SettingField::switch(
                        {
                            let view = view_entity.clone();
                            move |app| {
                                view.read(app)
                                    .snapshot
                                    .as_ref()
                                    .is_some_and(|s| s.motion_audio_enabled)
                            }
                        },
                        {
                            let view = view_entity.clone();
                            move |value, app| {
                                view.update(app, |view, cx| {
                                    view.set_motion_audio_enabled(value, cx)
                                });
                            }
                        },
                    ),
                ),
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.remember_last_expression.label",
                    ),
                    SettingField::switch(
                        {
                            let view = view_entity.clone();
                            move |app| {
                                view.read(app)
                                    .snapshot
                                    .as_ref()
                                    .is_some_and(|s| s.remember_last_expression)
                            }
                        },
                        {
                            let view = view_entity.clone();
                            move |value, app| {
                                view.update(app, |view, cx| {
                                    view.set_remember_last_expression(value, cx)
                                });
                            }
                        },
                    ),
                ),
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.toggle_repeated_expression.label",
                    ),
                    SettingField::switch(
                        {
                            let view = view_entity.clone();
                            move |app| {
                                view.read(app)
                                    .snapshot
                                    .as_ref()
                                    .is_some_and(|s| s.toggle_repeated_expression)
                            }
                        },
                        {
                            let view = view_entity.clone();
                            move |value, app| {
                                view.update(app, |view, cx| {
                                    view.set_toggle_repeated_expression(value, cx)
                                });
                            }
                        },
                    ),
                ),
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.allow_motion_overlap.label",
                    ),
                    SettingField::switch(
                        {
                            let view = view_entity.clone();
                            move |app| {
                                view.read(app)
                                    .snapshot
                                    .as_ref()
                                    .is_some_and(|s| s.allow_motion_overlap)
                            }
                        },
                        {
                            let view = view_entity.clone();
                            move |value, app| {
                                view.update(app, |view, cx| {
                                    view.set_allow_motion_overlap(value, cx)
                                });
                            }
                        },
                    ),
                ),
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.random_behavior_mode.label",
                    ),
                    SettingField::element({
                        let view = view_entity.clone();
                        move |options: &RenderOptions, window: &mut Window, app: &mut App| {
                            let state = view.read(app).random_behavior_mode_select.clone();
                            Select::new(&state)
                                .menu_width(dropdown_menu_width(
                                    window,
                                    random_behavior_mode_options(language),
                                ))
                                .disabled(options.is_disabled())
                                .into_any_element()
                        }
                    }),
                )
                .disabled(random_behavior_gate.disables_switch()),
                SettingItem::new(
                    bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.models.behavior.random_behavior_interval.label",
                    ),
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: f64::from(
                                bongocat_config::MINIMUM_RANDOM_BEHAVIOR_INTERVAL_SECONDS,
                            ),
                            max: f64::from(
                                bongocat_config::MAXIMUM_RANDOM_BEHAVIOR_INTERVAL_SECONDS,
                            ),
                            step: 1.0,
                        },
                        {
                            let view = view_entity.clone();
                            move |app| {
                                view.read(app).snapshot.as_ref().map_or(30.0, |snapshot| {
                                    f64::from(snapshot.random_behavior.interval_seconds)
                                })
                            }
                        },
                        {
                            let view = view_entity.clone();
                            move |value, app| {
                                view.update(app, |view, cx| {
                                    view.set_random_behavior_interval_value(value, cx)
                                });
                            }
                        },
                    ),
                )
                .disabled(random_behavior_gate.disables_controls()),
            ],
            &model_behavior_keywords,
        ));

        // Continuous device input and the model's response to it form one
        // task-oriented page. Discrete shortcut bindings stay separate below.
        let mouse_keywords = SettingsNavigationPage::InputInteraction
            .search_keywords(language, ["settings.input_interaction.mouse.title"]);
        let keyboard_keywords = SettingsNavigationPage::InputInteraction
            .search_keywords(language, ["settings.input_interaction.keyboard.title"]);
        let gamepad_keywords = SettingsNavigationPage::InputInteraction
            .search_keywords(language, ["settings.input_interaction.gamepad.title"]);
        let input_interaction_page =
            SettingPage::new(SettingsNavigationPage::InputInteraction.title(language))
                .icon(SettingsNavigationPage::InputInteraction.icon())
                .title_suffix(page_reporter(
                    SettingsNavigationPage::InputInteraction,
                    navigation_memory.clone(),
                ))
                .groups(vec![
            SettingGroup::new()
                .title(bongocat_i18n::text(
                    language.catalog_locale(),
                    "settings.input_interaction.mouse.title",
                ))
                .items(with_search_keywords(vec![
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.mouse.ignore_mouse_input.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.model_settings.ignore_pointer)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.model_settings;
                                            settings.ignore_pointer = value;
                                            view.set_model_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    ),
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.mouse.mirror_mouse_tracking_horizontal.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| {
                                            s.model_settings.mirror_pointer_tracking_horizontal
                                        })
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.model_settings;
                                            settings.mirror_pointer_tracking_horizontal = value;
                                            view.set_model_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    ),
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.mouse.mirror_mouse_tracking_vertical.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| {
                                            s.model_settings.mirror_pointer_tracking_vertical
                                        })
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.model_settings;
                                            settings.mirror_pointer_tracking_vertical = value;
                                            view.set_model_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    ),
                    // The last row is the one that changes how the pointer is
                    // read rather than what the model does with it, so it reads
                    // as a note on the three above: if the pointer does not move
                    // at all, none of them can have an effect.
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.mouse.force_move.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.cursor_settings.force_move)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.cursor_settings;
                                            settings.force_move = value;
                                            view.set_cursor_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    )
                    // The one row on this page whose title does not say when it
                    // applies. "Force mouse movement" reads as a behaviour the
                    // cat always has, and the whole point is that it only
                    // matters where the pointer is captured.
                    .description(bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.input_interaction.mouse.force_move.description",
                    )),
                ], &mouse_keywords)),
            SettingGroup::new()
                .title(bongocat_i18n::text(
                    language.catalog_locale(),
                    "settings.input_interaction.keyboard.title",
                ))
                .items(with_search_keywords(vec![
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.keyboard.ignore_keyboard_input.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.model_settings.ignore_keyboard)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.model_settings;
                                            settings.ignore_keyboard = value;
                                            view.set_model_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    ),
                    // After the gate, not before it: the gate decides which keys reach the
                    // key-image layer at all, and this decides how that layer
                    // draws them. The label carries the whole meaning, so the
                    // row stays as compact as its neighbours'.
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.keyboard.show_all_pressed_keys.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.model_settings.show_all_pressed_keys)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.model_settings;
                                            settings.show_all_pressed_keys = value;
                                            view.set_model_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    ),
                ], &keyboard_keywords)),
            SettingGroup::new()
                .title(bongocat_i18n::text(
                    language.catalog_locale(),
                    "settings.input_interaction.gamepad.title",
                ))
                .items(with_search_keywords(vec![
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.gamepad.ignore_gamepad_input.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.model_settings.ignore_gamepad)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        if let Some(s) = view.snapshot.as_ref() {
                                            let mut settings = s.model_settings;
                                            settings.ignore_gamepad = value;
                                            view.set_model_settings(settings, cx);
                                        }
                                    });
                                }
                            },
                        ),
                    ),
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.gamepad.stick_dead_zone.label",
                        ),
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.0,
                                max: 99.0,
                                step: 5.0,
                            },
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app).snapshot.as_ref().map_or(15.0, |s| {
                                        f64::from(s.gamepad_axis_settings.stick_dead_zone_percent)
                                    })
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_gamepad_dead_zone_value(true, value, cx)
                                    });
                                }
                            },
                        ),
                    )
                    .description(bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.input_interaction.gamepad.stick_dead_zone.description",
                    )),
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.gamepad.trigger_dead_zone.label",
                        ),
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.0,
                                max: 99.0,
                                step: 5.0,
                            },
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app).snapshot.as_ref().map_or(0.0, |s| {
                                        f64::from(s.gamepad_axis_settings.trigger_dead_zone_percent)
                                    })
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_gamepad_dead_zone_value(false, value, cx)
                                    });
                                }
                            },
                        ),
                    )
                    .description(bongocat_i18n::text(
                        language.catalog_locale(),
                        "settings.input_interaction.gamepad.trigger_dead_zone.description",
                    )),
                    // The switch that owns the two dropdowns sits directly above
                    // them and after the group's other rows: it is an addition to
                    // the group's everyday controls, and the two values it gates
                    // are its own subject.
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.input_interaction.gamepad.auto_switch.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.gamepad_auto_switch.enabled)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_gamepad_auto_switch_enabled(value, cx)
                                    });
                                }
                            },
                        ),
                    )
                    .disabled(gamepad_auto_switch_gate.disables_switch()),
                    gamepad_auto_switch_model_row(
                        "settings.input_interaction.gamepad.connected_model.label",
                        language,
                        gamepad_connected_choices,
                        &gamepad_connected_model_select,
                        gamepad_auto_switch_gate,
                    ),
                    gamepad_auto_switch_model_row(
                        "settings.input_interaction.gamepad.disconnected_model.label",
                        language,
                        gamepad_disconnected_choices,
                        &gamepad_disconnected_model_select,
                        gamepad_auto_switch_gate,
                    ),
                ], &gamepad_keywords)),
        ]);

        // The application's own surface: how it integrates with the desktop
        // and how much diagnostic logging it retains. There is no update
        // section: this build targets one panel on one machine and ships from
        // its own repository, so an update check would query a release feed
        // that has nothing to say about it.
        let app_desktop_keywords = SettingsNavigationPage::AppSystem
            .search_keywords(language, ["settings.app_system.desktop.title"]);
        let app_logging_keywords = SettingsNavigationPage::AppSystem
            .search_keywords(language, ["settings.app_system.logging.title"]);
        let app_system_page = SettingPage::new(SettingsNavigationPage::AppSystem.title(language))
            .icon(SettingsNavigationPage::AppSystem.icon())
            .title_suffix(page_reporter(
                SettingsNavigationPage::AppSystem,
                navigation_memory.clone(),
            ))
            .groups(vec![
            SettingGroup::new()
                .title(bongocat_i18n::text(
                    language.catalog_locale(),
                    "settings.app_system.desktop.title",
                ))
                .items(with_search_keywords({
                    // The login item is the first desktop preference because it
                    // describes what happens before the rest of the application.
                    let mut items = Vec::new();
                    let mut open_at_login = SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.app_system.open_at_login.label",
                        ),
                        SettingField::switch(move |_app| startup_item.enabled, {
                            let view = view_entity.clone();
                            move |enabled: bool, app: &mut App| {
                                view.update(app, |view, cx| {
                                    view.set_startup_item_enabled(enabled, cx)
                                });
                            }
                        }),
                    )
                    .disabled(startup_item.disabled);
                    if let Some(description) = startup_item.description {
                        open_at_login = open_at_login.description(description);
                    }
                    items.push(open_at_login);
                    items.push(SettingItem::new(
                        bongocat_i18n::platform_text(
                            language.catalog_locale(),
                            "settings.app_system.status_icon.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.status_icon_visible)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_status_icon_visible(value, cx)
                                    });
                                }
                            },
                        ),
                    ));
                    #[cfg(target_os = "windows")]
                    items.push(SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.app_system.taskbar_icon.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.taskbar_icon_visible || s.overlay.window_mode)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_taskbar_icon_visible(value, cx)
                                    });
                                }
                            },
                        ),
                    ).disabled(window_mode_active(snapshot.as_ref().map(|s| s.overlay))));
                    // The macOS counterpart of the Windows taskbar button. It is its
                    // own row and its own field rather than a second meaning of
                    // `show_taskbar_icon`, which the contract explicitly refuses to
                    // read as a Dock icon.
                    #[cfg(target_os = "macos")]
                    items.push(SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.app_system.dock_icon.label",
                        ),
                        SettingField::switch(
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app)
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.dock_icon_visible)
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_dock_icon_visible(value, cx)
                                    });
                                }
                            },
                        ),
                    ));
                    items
                }, &app_desktop_keywords)),
            SettingGroup::new()
                .title(bongocat_i18n::text(
                    language.catalog_locale(),
                    "settings.app_system.logging.title",
                ))
                .items(with_search_keywords(vec![
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.app_system.logging.level.label",
                        ),
                        SettingField::element({
                            let view = view_entity.clone();
                            move |_: &RenderOptions, window: &mut Window, app: &mut App| {
                                let state = view.read(app).logging_level_select.clone();
                                Select::new(&state)
                                    .menu_width(dropdown_menu_width(
                                        window,
                                        logging_level_options(language),
                                    ))
                                    .disabled(editing_blocked)
                                    .into_any_element()
                            }
                        }),
                    )
                    .disabled(editing_blocked),
                    SettingItem::new(
                        bongocat_i18n::text(
                            language.catalog_locale(),
                            "settings.app_system.logging.retention_days.label",
                        ),
                        SettingField::number_input(
                            logging_retention_number_field_options(),
                            {
                                let view = view_entity.clone();
                                move |app| {
                                    view.read(app).snapshot.as_ref().map_or(
                                        f64::from(SettingsLogging::default().retention_days),
                                        |snapshot| f64::from(snapshot.logging.retention_days),
                                    )
                                }
                            },
                            {
                                let view = view_entity.clone();
                                move |value, app| {
                                    view.update(app, |view, cx| {
                                        view.set_logging_retention_days_value(value, cx)
                                    });
                                }
                            },
                        ),
                    )
                    .disabled(editing_blocked),
                ], &app_logging_keywords)),
        ]);

        // The model library is a flat grid of self-drawn cards, so its page drops
        // the window-wide Outline surface. Model behavior is a separate page and
        // keeps the normal settings-card surface; neither page shares a body or
        // a scroll region with the other.
        let model_library_keywords = model_library_search_keywords(
            language,
            snapshot
                .as_ref()
                .into_iter()
                .flat_map(|snapshot| snapshot.model_catalog.entries.iter())
                .map(|entry| entry.title.clone()),
        );
        let model_library_group = SettingGroup::new().variant(GroupBoxVariant::Normal).item(
            SettingItem::render({
                let view = view_entity.clone();
                move |_: &RenderOptions, window: &mut Window, app: &mut App| {
                    let snapshot = view.read(app).snapshot.clone();
                    let tokens = Tokens::from_theme(app);
                    view.update(app, move |view, cx| {
                        models::content(view, window, cx, snapshot.as_ref(), tokens)
                    })
                    .into_any_element()
                }
            })
            .keywords(model_library_keywords),
        );
        let model_library_page =
            SettingPage::new(SettingsNavigationPage::ModelLibrary.title(language))
                .icon(SettingsNavigationPage::ModelLibrary.icon())
                .title_suffix(page_reporter(
                    SettingsNavigationPage::ModelLibrary,
                    navigation_memory.clone(),
                ))
                .group(model_library_group);
        let model_behavior_page =
            SettingPage::new(SettingsNavigationPage::ModelBehavior.title(language))
                .icon(SettingsNavigationPage::ModelBehavior.icon())
                .title_suffix(page_reporter(
                    SettingsNavigationPage::ModelBehavior,
                    navigation_memory.clone(),
                ))
                .group(model_behavior_group);

        // The page's two scopes are two titled groups rather than tabs: a
        // `SettingPage` cannot host child pages, but the settings component
        // renders every titled group of a page with more than one group as a
        // second-level sidebar entry, so both scopes stay directly reachable
        // from the sidebar and the body renders them one after the other.
        let shortcuts_page = SettingPage::new(SettingsNavigationPage::Shortcuts.title(language))
            .icon(SettingsNavigationPage::Shortcuts.icon())
            .title_suffix(page_reporter(
                SettingsNavigationPage::Shortcuts,
                navigation_memory.clone(),
            ))
            .groups(vec![
                shortcuts_page::group(
                    shortcuts_page::ShortcutScope::Window,
                    language,
                    view_entity.clone(),
                    command_shortcuts_gate,
                ),
                shortcuts_page::group(
                    shortcuts_page::ShortcutScope::Model,
                    language,
                    view_entity.clone(),
                    behavior_shortcuts_gate,
                ),
            ]);

        // About is the final normal settings destination. Its operational rows
        // use the same SettingGroup contract as the rest of the settings
        // window; the page intentionally stays focused on product information
        // and support actions.
        let about_keywords =
            SettingsNavigationPage::About.search_keywords(language, std::iter::empty());

        // Plugins page shows the plugin management interface.
        let plugins_keywords =
            SettingsNavigationPage::Plugins.search_keywords(language, std::iter::empty());
        let plugins_group = SettingGroup::new().variant(GroupBoxVariant::Normal).item(
            SettingItem::render({
                let view = view_entity.clone();
                move |_: &RenderOptions, window: &mut Window, app: &mut App| {
                    let page_language = view.read(app).display_language();
                    view.update(app, move |view, cx| {
                        plugins_page::content(view, window, page_language, cx)
                    })
                    .into_any_element()
                }
            })
            .keywords(plugins_keywords),
        );
        let plugins_page = SettingPage::new(SettingsNavigationPage::Plugins.title(language))
            .icon(SettingsNavigationPage::Plugins.icon())
            .title_suffix(page_reporter(
                SettingsNavigationPage::Plugins,
                navigation_memory.clone(),
            ))
            .group(plugins_group);

        let about_page = SettingPage::new(SettingsNavigationPage::About.title(language))
            .icon(SettingsNavigationPage::About.icon())
            .title_suffix(page_reporter(
                SettingsNavigationPage::About,
                navigation_memory.clone(),
            ))
            .group(about::operational_group(
                view_entity.clone(),
                snapshot.as_ref(),
                language,
                about_keywords,
                self.request_update.clone(),
            ));

        let settings = Settings::new("bongocat-settings")
            .sidebar_width(px(220.0))
            .default_selected_index(SelectIndex {
                page_ix: self.navigation_memory.page_index(),
                group_ix: None,
            })
            .with_group_variant(GroupBoxVariant::Outline)
            .pages(vec![
                appearance_page,
                model_library_page,
                model_behavior_page,
                device_screen_page,
                input_interaction_page,
                shortcuts_page,
                plugins_page,
                app_system_page,
                about_page,
            ]);
        let model_drag_overlay = self.model_drag.map(|state| {
            let view = cx.entity().downgrade();
            model_drag_overlay::render(state, language, Tokens::from_theme(cx))
                .on_drag_move(
                    cx.listener(|view, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let path_count = event.drag(cx).paths().len();
                        view.update_model_drag(path_count, cx);
                    }),
                )
                .on_drop({
                    let view = view.clone();
                    move |paths: &ExternalPaths, _, cx| {
                        let _ = view.update(cx, |view, cx| {
                            view.accept_model_folder_drop(paths.paths(), cx);
                        });
                    }
                })
        });

        div()
            .id("bongocat-settings-root")
            .relative()
            .on_drag_move(
                cx.listener(|view, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    let path_count = event.drag(cx).paths().len();
                    view.update_model_drag(path_count, cx);
                }),
            )
            .on_drop(cx.listener(|view, paths: &ExternalPaths, _, cx| {
                view.accept_model_folder_drop(paths.paths(), cx);
            }))
            .on_key_down(cx.listener(|view, event, window, cx| {
                if view.shortcut_capture.is_some() {
                    cx.stop_propagation();
                    view.capture_shortcut(event, window, cx);
                }
            }))
            .on_key_up(cx.listener(|view, event, window, cx| {
                view.update_shortcut_capture_on_key_up(event, window, cx);
            }))
            .on_modifiers_changed(cx.listener(
                |view, event: &gpui_kit::ModifiersChangedEvent, window, cx| {
                    view.update_shortcut_capture_modifiers(event.modifiers, window, cx);
                },
            ))
            .size_full()
            .flex()
            .flex_col()
            .child(div().min_h_0().w_full().flex_1().child(settings))
            .child(model_drag_exit_listener)
            .children(model_drag_overlay)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_reporter_updates_the_process_navigation_memory() {
        let memory = SettingsNavigationMemory::new();
        report_navigation_page(SettingsNavigationPage::ModelBehavior, &memory);
        assert_eq!(memory.page_index(), 2);
    }
}
