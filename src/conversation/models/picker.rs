use super::*;
use gpui_component::{list::List, menu::PopupMenuItem, popover::Popover, switch::Switch};

const PICKER_WIDTH: Pixels = px(476.);
const PICKER_BODY_HEIGHT: Pixels = px(210.);
const PROVIDER_SIDEBAR_WIDTH: Pixels = px(136.);
// Allow for the sidebar insets and divider, plus the button's horizontal padding.
const PROVIDER_LABEL_WIDTH: Pixels = px(95.);

impl Desktop {
    pub(in crate::conversation) fn model_control(&self, cx: &Context<Self>) -> Div {
        let (model, options) = self.model_selection();
        let source = self.picker_source(cx);
        let driver = self
            .runtime
            .provider_instances()
            .find(|provider| provider.id == source.0)
            .map(|provider| provider.driver);
        let catalog = self
            .model_catalogs
            .get(&source)
            .map(|catalog| catalog.read(cx));
        let report = catalog.and_then(ProviderCatalog::report);
        let details = model_details(report, model);
        let display_id = model.or_else(|| reported_default_model(report));
        let effort = effective_effort(options, details);
        let name = display_name(driver, display_id, model_name(model, details));
        let trigger = Button::new("choose-model")
            .small()
            .ghost()
            .dropdown_caret(true)
            .tooltip("Choose model")
            .child(
                h_flex()
                    .gap_1p5()
                    .min_w_0()
                    .child(model_logo(driver, display_id, cx))
                    .child(div().max_w(px(176.)).truncate().child(name))
                    .when_some(effort, |row, effort| {
                        row.child(hint(effort_label(effort), cx).flex_shrink_0())
                    }),
            )
            .on_click(cx.listener(|view, event: &ClickEvent, window, cx| {
                if event.is_keyboard() {
                    if view.model_picker.is_some() {
                        view.close_model_picker(window, cx);
                    } else {
                        view.open_models(false, window, cx);
                    }
                }
            }));
        let picker = Popover::new("model-picker")
            .anchor(Corner::BottomLeft)
            .p_0()
            .trigger(trigger)
            .open(self.model_picker.is_some())
            .on_open_change(cx.listener(|view, open, window, cx| {
                if *open {
                    view.open_models(false, window, cx);
                } else {
                    view.close_model_picker(window, cx);
                }
            }));
        let picker = if let Some(list) = &self.model_picker {
            let models = list.read(cx).delegate();
            let configured: Vec<_> = self.configured_providers(cx).collect();
            let active_configured = configured.iter().any(|provider| provider.id == source.0);
            let providers = configured.into_iter().map(|instance| {
                let id = instance.id.clone();
                let can_select = self.current.is_none() || id == source.0;
                let name = self.runtime.output().provider_label(id.as_str()).to_owned();
                control!(
                    format!("provider/{name}"),
                    Button::new(SharedString::from(format!("provider-{id}")))
                        .small()
                        .ghost()
                        .w_full()
                        .justify_start()
                        .selected(id == source.0 && !models.favorites_open)
                        .disabled(!can_select)
                        .tooltip(if can_select {
                            name.clone()
                        } else {
                            format!("{name} is available in new conversations")
                        })
                        .child(
                            h_flex()
                                .w(PROVIDER_LABEL_WIDTH)
                                .gap_1()
                                .min_w_0()
                                .child(provider_logo(instance.driver, px(16.), cx))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .child(div().w_full().truncate().child(name))
                                )
                        )
                        .on_click(cx.listener(move |view, _, window, cx| {
                            if view.current.is_none() {
                                view.choose_provider(&id, cx);
                            }
                            view.open_models(false, window, cx);
                        }))
                )
            });
            let sidebar = v_flex()
                .w(PROVIDER_SIDEBAR_WIDTH)
                .h_full()
                .flex_shrink_0()
                .p_2()
                .bg(cx.theme().sidebar)
                .border_r_1()
                .border_color(cx.theme().border)
                .child(control!(
                    "Favorites",
                    Button::new("model-picker-favorites")
                        .small()
                        .ghost()
                        .w_full()
                        .justify_start()
                        .icon(IconName::Star)
                        .label("Favorites")
                        .selected(models.favorites_open)
                        .on_click(
                            cx.listener(|view, _, window, cx| view.open_models(true, window, cx))
                        )
                ))
                .child(div().my_1().h(px(1.)).bg(cx.theme().border))
                .child(
                    v_flex()
                        .id("model-picker-providers")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(providers),
                );
            let status = if models.favorites_open {
                None
            } else {
                match (active_configured, catalog.map(|catalog| &catalog.result)) {
                    (false, _)
                        if self.current.is_none()
                            && self.configured_providers(cx).next().is_some() =>
                    {
                        Some(("Choose a provider to load models.".into(), false))
                    }
                    (false, _) => (catalog
                        .filter(|catalog| catalog.login_required())
                        .and_then(|catalog| catalog.result.as_ref())
                        .and_then(|result| result.as_ref().err()))
                    .map(|error| (error.to_string(), true))
                    .or_else(|| Some(("Connect a provider in Settings.".into(), false))),
                    (_, None | Some(None)) => Some(("Loading models…".into(), false)),
                    (_, Some(Some(Err(error)))) => Some((error.to_string(), true)),
                    (_, Some(Some(Ok(report)))) if report.models.is_empty() => {
                        Some(("This provider listed no models.".into(), true))
                    }
                    _ => None,
                }
            };
            let active = source.0;
            let body = v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .p_2()
                .gap_1()
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .items_start()
                        .gap_1()
                        .child(
                            control!(
                                "Model search and results",
                                List::new(list)
                                    .search_placeholder(if models.favorites_open {
                                        "Search favorites"
                                    } else {
                                        "Search models"
                                    })
                                    .size_full()
                            )
                            .flex_1()
                            .min_w_0()
                            .h_full(),
                        )
                        .when(!models.favorites_open, |row| {
                            row.child(
                                control!(
                                    "Edit",
                                    Button::new("edit-provider-settings")
                                        .small()
                                        .ghost()
                                        .icon(IconName::Settings)
                                        .tooltip("Edit provider settings")
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.open_settings(
                                                SettingsTarget::Provider(&active),
                                                window,
                                                cx,
                                            );
                                        }))
                                )
                                .flex_shrink_0(),
                            )
                        }),
                )
                .when_some(status, |body, (message, retry)| {
                    body.child(
                        h_flex()
                            .gap_2()
                            .px_1()
                            .justify_between()
                            .child(control!(
                                format!("model-status/{message}"),
                                hint(message, cx).min_w_0().max_h(px(42.)).overflow_hidden()
                            ))
                            .when(retry, |row| {
                                row.child(
                                    control!(
                                        "Retry models",
                                        Button::new("retry-models")
                                            .small()
                                            .ghost()
                                            .label("Retry")
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.catalog_for(view.picker_source(cx), true, cx);
                                                view.open_models(false, window, cx);
                                            }))
                                    )
                                    .flex_shrink_0(),
                                )
                            }),
                    )
                });
            picker.child(
                v_flex()
                    .w(PICKER_WIDTH)
                    .child(h_flex().h(PICKER_BODY_HEIGHT).child(sidebar).child(body))
                    .child(self.model_options(
                        models.reasoning_menu.as_ref(),
                        report,
                        model,
                        details,
                        options,
                        cx,
                    )),
            )
        } else {
            picker
        };
        control!(
            model.map_or_else(|| "model-automatic".into(), |id| format!("model/{id}")),
            picker
        )
    }

    fn model_options(
        &self,
        reasoning_menu: Option<&Entity<PopupMenu>>,
        report: Option<&ProviderReport>,
        model: Option<&str>,
        details: Option<&ModelDetails>,
        options: &ModelOptions,
        cx: &Context<Self>,
    ) -> Div {
        let current_effort = effective_effort(options, details);
        let efforts = details.map_or(&[][..], |details| &details.efforts);
        let can_reason = !efforts.is_empty();
        let choices = efforts
            .iter()
            .map(|effort| (effort.clone(), current_effort == Some(effort)))
            .collect::<Vec<_>>();
        let menu_open = reasoning_menu.is_some();
        let reasoning = h_flex()
            .relative()
            .child(control!(
                if can_reason {
                    "Effort"
                } else {
                    "Reasoning unavailable"
                },
                Button::new("model-effort")
                    .small()
                    .ghost()
                    .disabled(!can_reason)
                    .selected(menu_open)
                    .dropdown_caret(true)
                    .child(
                        h_flex().gap_1p5().child("Reasoning").child(hint(
                            current_effort
                                .map(effort_label)
                                .unwrap_or_else(|| if can_reason {
                                    "Choose level".into()
                                } else {
                                    "Unavailable".into()
                                }),
                            cx,
                        ))
                    )
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.set_reasoning_menu(
                            (!menu_open).then_some(choices.as_slice()),
                            window,
                            cx,
                        );
                    }))
            ))
            .when_some(reasoning_menu, |trigger, menu| {
                // Render within the picker overlay so GPUI only defers the outer popover.
                trigger.child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom(px(28.))
                        .occlude()
                        .child(menu.clone()),
                )
            });
        let fast_tier = details.and_then(|details| details.fast_tier.clone());
        let speed = effective_speed(options, details);
        let fast = fast_tier.is_some() && speed.is_some_and(is_fast_tier);
        let speed_control = control!(
            match (fast_tier.is_some(), fast) {
                (false, _) => "Fast mode unavailable",
                (true, true) => "Fast mode on",
                (true, false) => "Fast mode off",
            },
            Button::new("model-speed")
                .small()
                .ghost()
                .disabled(fast_tier.is_none())
                .tooltip(if fast_tier.is_some() {
                    "Toggle fast mode"
                } else {
                    "Fast mode is unavailable for this model"
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child("Fast mode")
                        .child(Switch::new("model-fast-mode").small().checked(fast))
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.update_model_options(
                        |options| {
                            options.service_tier = if fast {
                                Some(STANDARD_SPEED.into())
                            } else {
                                fast_tier.clone()
                            };
                        },
                        cx,
                    );
                }))
        );
        let favorite = report.and_then(|report| {
            let model = model.or_else(|| reported_default_model(Some(report)))?;
            let provider = self.picker_source(cx).0;
            let selection = ModelFavorite {
                provider,
                model: model.into(),
                name: model_name(Some(model), details).into(),
                options: options.clone(),
            };
            let snapshot = ModelFavorite {
                options: model_snapshot(options, details),
                ..selection.clone()
            };
            snapshot.supported_by(report).then(|| {
                let saved = self.settings.read(cx).model_favorites();
                if saved
                    .iter()
                    .any(|favorite| favorite.same_selection(&selection))
                {
                    // A migrated favorite can still inherit the provider's defaults.
                    (selection, true)
                } else {
                    let already_saved = saved
                        .iter()
                        .any(|favorite| favorite.same_selection(&snapshot));
                    (snapshot, already_saved)
                }
            })
        });
        let saved = favorite.as_ref().is_some_and(|(_, saved)| *saved);
        let (label, tooltip) = match (favorite.is_some(), saved) {
            (true, true) => ("Saved", "Remove this combination from Favorites"),
            (true, false) => ("Save favorite", "Save this model, reasoning, and fast mode"),
            _ => (
                "Save favorite",
                "Choose an available model to save a favorite",
            ),
        };
        let favorite_button = control!(
            if saved {
                "Remove favorite"
            } else {
                "Save favorite"
            },
            Button::new("toggle-model-favorite")
                .small()
                .ghost()
                .selected(saved)
                .disabled(favorite.is_none())
                .icon(if saved {
                    IconName::Check
                } else {
                    IconName::Star
                })
                .label(label)
                .tooltip(tooltip)
                .on_click(cx.listener(move |view, _, window, cx| {
                    if let Some((favorite, _)) = favorite.clone() {
                        view.set_model_favorite(favorite, !saved, window, cx);
                    }
                }))
        );
        h_flex()
            .justify_between()
            .px_2()
            .py_1p5()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(h_flex().gap_1().child(speed_control).child(reasoning))
            .child(favorite_button)
    }

    fn set_reasoning_menu(
        &mut self,
        choices: Option<&[(String, bool)]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(list) = self.model_picker.clone() else {
            return;
        };
        let focus = list.focus_handle(cx);
        let menu = choices.map(|choices| {
            let view = cx.weak_entity();
            let menu = PopupMenu::build(window, cx, |menu, _, _| {
                choices.iter().fold(
                    menu.max_h(px(182.))
                        .scrollable(true)
                        .action_context(focus.clone()),
                    |menu, (effort, selected)| {
                        let label = effort_label(effort);
                        let effort = effort.clone();
                        let view = view.clone();
                        #[cfg(all(feature = "qualification", target_os = "macos"))]
                        let key = format!("effort/{effort}");
                        menu.item(
                            PopupMenuItem::element(move |_, _| {
                                control!(key.clone(), div().child(label.clone()))
                            })
                            .checked(*selected)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |view, cx| {
                                    view.update_model_options(
                                        |options| options.effort = Some(effort.clone()),
                                        cx,
                                    );
                                });
                            }),
                        )
                    },
                )
            });
            cx.subscribe(&menu, |view, menu, _: &DismissEvent, cx| {
                if let Some(list) = &view.model_picker {
                    list.update(cx, |list, cx| {
                        if list.delegate().reasoning_menu.as_ref() == Some(&menu) {
                            list.delegate_mut().reasoning_menu = None;
                            cx.notify();
                        }
                    });
                    cx.notify();
                }
            })
            .detach();
            menu.focus_handle(cx).focus(window);
            menu
        });
        if menu.is_none() {
            focus.focus(window);
        }
        list.update(cx, |list, cx| {
            list.delegate_mut().reasoning_menu = menu;
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn set_model_favorite(
        &mut self,
        favorite: ModelFavorite,
        saved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let persisted = self.settings.update(cx, |settings, cx| {
            settings.set_model_favorite(favorite, saved, cx)
        });
        if persisted {
            self.error = None;
            if let Some(list) = &self.model_picker
                && list.read(cx).delegate().favorites_open
            {
                self.open_models(true, window, cx);
            }
        } else {
            self.report_error("Could not save model favorites.".into(), cx);
        }
        cx.notify();
    }
}
