use super::*;
use crate::conversation::effort_label;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use huggler::{ModelOptions, ProviderReport};

pub(crate) const STANDARD_SPEED: &str = "default";

pub(super) type Slots = [Option<ModelPreset>; 5];

pub(super) fn all_empty(slots: &Slots) -> bool {
    slots.iter().all(Option::is_none)
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ModelPreset {
    pub provider: ProviderInstanceId,
    pub model: String,
    pub name: String,
    pub options: ModelOptions,
}

impl ModelPreset {
    pub(crate) fn supported_by(&self, report: &ProviderReport) -> bool {
        if !report.models.contains(&self.model) {
            return false;
        }
        let details = report.model_details.get(&self.model);
        self.options
            .effort
            .as_ref()
            .is_none_or(|effort| details.is_some_and(|details| details.efforts.contains(effort)))
            && self.options.service_tier.as_deref().is_none_or(|tier| {
                details
                    .and_then(|details| details.fast_tier.as_deref())
                    .is_some_and(|fast| tier == STANDARD_SPEED || tier == fast)
            })
    }

    pub(crate) fn speed_label(tier: Option<&str>) -> &'static str {
        match tier {
            None => "Default",
            Some(STANDARD_SPEED) => "Standard",
            Some("fast" | "priority") => "Fast",
            Some(_) => "Unavailable",
        }
    }
}

impl Settings {
    pub(crate) fn enabled_presets(&self) -> impl Iterator<Item = (usize, &ModelPreset)> {
        self.prefs
            .default_models
            .iter()
            .enumerate()
            .filter_map(|(index, preset)| {
                let preset = preset.as_ref()?;
                self.prefs
                    .models
                    .get(preset.provider.as_str())
                    .is_none_or(|models| models.enabled(&preset.model))
                    .then_some((index, preset))
            })
    }

    pub(super) fn request_preset_catalogs(&self, refresh: bool, cx: &mut Context<Self>) {
        for provider in self.runtime.provider_instances() {
            if self.provider_configured(provider) {
                cx.emit(SettingsEvent::ProviderModels(provider.id.clone(), refresh));
            }
        }
    }

    fn save_preset(&mut self, index: usize, preset: Option<ModelPreset>, cx: &mut Context<Self>) {
        let mut prefs = self.prefs.clone();
        prefs.default_models[index] = preset;
        self.persist(prefs, cx);
    }

    pub(super) fn default_models_panel(&self, cx: &Context<Self>) -> Div {
        let providers: Vec<_> = self
            .runtime
            .provider_instances()
            .filter(|provider| self.provider_configured(provider))
            .collect();
        let mut models = Vec::new();
        let mut loading = false;
        let mut unavailable = Vec::new();
        for provider in &providers {
            let name = self.runtime.output().provider_label(provider.id.as_str());
            let catalog = self.preset_catalogs.get(provider.id.as_str());
            match catalog.and_then(|(catalog, _)| catalog.read(cx).result.as_ref()) {
                None => loading = true,
                Some(Err(_)) => unavailable.push(name.to_owned()),
                Some(Ok(report)) => {
                    let prefs = self.model_preferences(&provider.id);
                    models.extend(
                        prefs
                            .ordered(&report.models)
                            .into_iter()
                            .filter(|id| prefs.enabled(id))
                            .map(|id| {
                                let model = report.model_details.get(id);
                                let name = model.map_or(id, |details| details.name.as_str());
                                (
                                    format!(
                                        "{name} · {}",
                                        self.runtime.output().provider_label(provider.id.as_str())
                                    ),
                                    ModelPreset {
                                        provider: provider.id.clone(),
                                        model: id.to_owned(),
                                        name: name.to_owned(),
                                        options: ModelOptions::default(),
                                    },
                                )
                            }),
                    );
                }
            }
        }
        let status = if providers.is_empty() {
            Some("Connect a provider in Settings to choose a model.".to_owned())
        } else if loading {
            Some("Loading models…".to_owned())
        } else if !unavailable.is_empty() {
            Some(format!(
                "Could not load models for {}.",
                unavailable.join(", ")
            ))
        } else if models.is_empty() {
            Some("No models are available. Manage visible models in Providers.".to_owned())
        } else {
            None
        };

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        hint(
                            "Saved presets appear in the message model picker when available.",
                            cx,
                        )
                        .min_w_0(),
                    )
                    .when(!providers.is_empty(), |row| {
                        row.child(
                            Button::new("refresh-preset-models")
                                .small()
                                .ghost()
                                .label("Refresh models")
                                .disabled(loading)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.request_preset_catalogs(true, cx)
                                })),
                        )
                    }),
            )
            .when_some(status, |panel, status| panel.child(hint(status, cx)))
            .child(
                v_flex()
                    .rounded_lg()
                    .overflow_hidden()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .children(self.prefs.default_models.iter().enumerate().map(
                        |(index, preset)| self.preset_row(index, preset.as_ref(), &models, cx),
                    )),
            )
    }

    fn preset_row(
        &self,
        index: usize,
        preset: Option<&ModelPreset>,
        models: &[(String, ModelPreset)],
        cx: &Context<Self>,
    ) -> Div {
        let number = index + 1;
        let model_label = preset.map_or_else(
            || "Choose model".to_owned(),
            |preset| {
                format!(
                    "{} · {}",
                    preset.name,
                    self.runtime
                        .output()
                        .provider_label(preset.provider.as_str())
                )
            },
        );
        let details = preset.and_then(|preset| {
            let (catalog, _) = self.preset_catalogs.get(preset.provider.as_str())?;
            catalog.read(cx).report()?.model_details.get(&preset.model)
        });
        let mut efforts = vec![("Default".into(), None)];
        let mut speeds = vec![("Default".into(), None)];
        if let Some(details) = details {
            efforts.extend(
                details
                    .efforts
                    .iter()
                    .map(|value| (effort_label(value), Some(value.clone()))),
            );
            if let Some(tier) = &details.fast_tier {
                speeds.push(("Standard".into(), Some(STANDARD_SPEED.into())));
                speeds.push(("Fast".into(), Some(tier.clone())));
            }
        }

        let model_choices = models
            .iter()
            .map(|(name, choice)| {
                let selected = preset.is_some_and(|preset| {
                    preset.provider == choice.provider && preset.model == choice.model
                });
                (name.clone(), selected, Some(choice.clone()))
            })
            .collect();
        let option_control =
            |field: &str, label: String, choices: Vec<(String, Option<String>)>, speed: bool| {
                let saved = preset.cloned();
                let selected = preset.and_then(|preset| {
                    if speed {
                        preset.options.service_tier.as_ref()
                    } else {
                        preset.options.effort.as_ref()
                    }
                });
                let enabled = choices.len() > 1 || selected.is_some();
                let choices = choices
                    .into_iter()
                    .map(|(name, value)| {
                        let checked = selected == value.as_ref();
                        let choice = saved.clone().map(|mut preset| {
                            if speed {
                                preset.options.service_tier = value;
                            } else {
                                preset.options.effort = value;
                            }
                            preset
                        });
                        (name, checked, choice)
                    })
                    .collect();
                Self::preset_field(index, field, label, enabled, choices, cx).w(px(96.))
            };

        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .when(index > 0, |row| {
                row.border_t_1().border_color(cx.theme().border)
            })
            .child(label(format!("Preset {number}")).w(px(56.)).flex_shrink_0())
            .child(
                Self::preset_field(
                    index,
                    "Model",
                    model_label,
                    !models.is_empty(),
                    model_choices,
                    cx,
                )
                .flex_1()
                .min_w_0(),
            )
            .child(option_control(
                "Effort",
                preset
                    .and_then(|p| p.options.effort.as_deref())
                    .map_or_else(|| "Default".into(), effort_label),
                efforts,
                false,
            ))
            .child(option_control(
                "Speed",
                ModelPreset::speed_label(preset.and_then(|p| p.options.service_tier.as_deref()))
                    .into(),
                speeds,
                true,
            ))
            .child(control!(
                format!("Clear preset {number}"),
                Button::new(SharedString::from(format!("clear-preset-{number}")))
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(format!("Clear preset {number}"))
                    .disabled(preset.is_none())
                    .mt_4()
                    .on_click(cx.listener(move |view, _, _, cx| view.save_preset(index, None, cx)))
            ))
    }

    fn preset_field(
        index: usize,
        field: &str,
        value: String,
        enabled: bool,
        choices: Vec<(String, bool, Option<ModelPreset>)>,
        cx: &Context<Self>,
    ) -> Div {
        let key = SharedString::from(format!("Preset {}/{field}", index + 1));
        let view = cx.weak_entity();
        #[cfg(all(feature = "qualification", target_os = "macos"))]
        let menu_key = key.clone();
        v_flex()
            .gap_1()
            .child(label(field.to_owned()))
            .child(control!(
                if enabled {
                    key.clone()
                } else {
                    SharedString::from(format!("{key}/disabled"))
                },
                Button::new(key)
                    .small()
                    .outline()
                    .w_full()
                    .justify_start()
                    .disabled(!enabled)
                    .dropdown_caret(true)
                    .tooltip(value.clone())
                    .child(div().flex_1().min_w_0().truncate().child(value))
                    .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                        choices.iter().fold(
                            menu.max_h(px(220.)).scrollable(true),
                            |menu, (name, selected, choice)| {
                                let view = view.clone();
                                let name = name.clone();
                                #[cfg(all(feature = "qualification", target_os = "macos"))]
                                let control_key = format!("{menu_key}/{name}");
                                let choice = choice.clone();
                                let selected = *selected;
                                menu.item(
                                    PopupMenuItem::element(move |_, _| {
                                        control!(control_key.clone(), div().child(name.clone()))
                                    })
                                    .checked(selected)
                                    .on_click(
                                        move |_, _, cx| {
                                            if !selected {
                                                let _ = view.update(cx, |view, cx| {
                                                    view.save_preset(index, choice.clone(), cx)
                                                });
                                            }
                                        },
                                    ),
                                )
                            },
                        )
                    })
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::ModelPreset;
    use huggler::{ModelDetails, ModelOptions, ProviderReport};
    use std::collections::BTreeMap;

    #[test]
    fn a_preset_requires_the_saved_model_and_options() {
        let mut preset = ModelPreset {
            provider: "codex".into(),
            model: "fixture".into(),
            name: "Fixture".into(),
            options: ModelOptions {
                effort: Some("high".into()),
                service_tier: Some("priority".into()),
            },
        };
        let mut report = ProviderReport {
            transport: "fixture".into(),
            version: None,
            authenticated: None,
            models: vec![preset.model.clone()],
            model_details: BTreeMap::from([(
                preset.model.clone(),
                ModelDetails {
                    efforts: vec!["high".into()],
                    fast_tier: Some("priority".into()),
                    ..Default::default()
                },
            )]),
            resume: true,
            agent_tools: true,
        };
        assert!(preset.supported_by(&report));

        report
            .model_details
            .get_mut(&preset.model)
            .unwrap()
            .fast_tier = None;
        assert!(!preset.supported_by(&report));
        preset.options.service_tier = None;
        assert!(preset.supported_by(&report));

        report.model_details.clear();
        assert!(!preset.supported_by(&report));
        preset.options.effort = None;
        assert!(preset.supported_by(&report));

        report.models.clear();
        assert!(!preset.supported_by(&report));
    }
}
