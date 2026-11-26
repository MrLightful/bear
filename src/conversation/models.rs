mod picker;
mod presentation;

use super::*;
use crate::settings::{
    ModelFavorite, ModelPreferences, STANDARD_SPEED, is_fast_tier, provider_logo,
};
use gpui_component::{
    IndexPath,
    list::{ListDelegate, ListEvent, ListItem, ListState},
    menu::PopupMenu,
};
use huggler::{ModelDetails, ProviderReport};
use presentation::{display_name, model_logo};

pub(crate) struct ProviderCatalog {
    instance: ProviderInstanceId,
    checkout: PathBuf,
    pub(crate) result: Option<huggler::Result<ProviderReport>>,
}

impl ProviderCatalog {
    pub(super) fn loading(&self) -> bool {
        self.result.is_none()
    }

    fn login_required(&self) -> bool {
        matches!(&self.result, Some(Err(huggler::Error::LoginRequired)))
    }

    pub(crate) fn report(&self) -> Option<&ProviderReport> {
        self.result.as_ref()?.as_ref().ok()
    }

    pub(super) fn status(&self) -> String {
        match &self.result {
            None => "Checking provider…".into(),
            Some(Err(error)) => error.to_string(),
            Some(Ok(report)) => format!(
                "{} · {} · {} · {} models",
                report.transport,
                report.version.as_deref().unwrap_or("version unknown"),
                if report.authenticated == Some(true) {
                    "signed in"
                } else {
                    "login unverified"
                },
                report.models.len(),
            ),
        }
    }

    fn load(&mut self, runtime: Arc<Huggler>, cx: &mut Context<Self>) {
        let instance = self.instance.clone();
        let checkout = self.checkout.clone();
        self.result = None;
        cx.notify();
        let job = cx.background_spawn(async move {
            runtime.probe_provider(instance.as_str(), checkout).await
        });
        cx.spawn(async move |catalog, cx| {
            let result = job.await;
            let _ = catalog.update(cx, |catalog, cx| {
                catalog.result = Some(result);
                cx.notify();
            });
        })
        .detach();
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ModelChoice {
    Favorite(ModelFavorite),
    Model(String),
}

pub(super) struct Models {
    source: ProviderInstanceId,
    desktop: WeakEntity<Desktop>,
    providers: HashMap<ProviderInstanceId, (Provider, String)>,
    allow_provider_switch: bool,
    catalog: Option<Entity<ProviderCatalog>>,
    current: Option<String>,
    options: ModelOptions,
    favorites_open: bool,
    reasoning_menu: Option<Entity<PopupMenu>>,
    saved: Vec<ModelFavorite>,
    favorite_preferences: HashMap<ProviderInstanceId, ModelPreferences>,
    catalogs: HashMap<ProviderInstanceId, Entity<ProviderCatalog>>,
    query: String,
    matches: Vec<ModelChoice>,
    preferences: ModelPreferences,
}

pub(super) fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra high".into(),
        _ => {
            let mut chars = effort.chars();
            chars
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .chain(chars)
                .collect()
        }
    }
}

fn model_details<'a>(
    report: Option<&'a ProviderReport>,
    model: Option<&str>,
) -> Option<&'a ModelDetails> {
    let models = &report?.model_details;
    match model {
        Some(id) => models.get(id),
        None => models.values().find(|model| model.is_default),
    }
}

fn reported_default_model(report: Option<&ProviderReport>) -> Option<&str> {
    report?
        .model_details
        .iter()
        .find_map(|(id, details)| details.is_default.then_some(id.as_str()))
}

fn model_name<'a>(model: Option<&'a str>, details: Option<&'a ModelDetails>) -> &'a str {
    details.map_or_else(|| model.unwrap_or("Choose model"), |details| &details.name)
}

fn effective_effort<'a>(
    options: &'a ModelOptions,
    details: Option<&'a ModelDetails>,
) -> Option<&'a str> {
    options
        .effort
        .as_deref()
        .or_else(|| details?.default_effort.as_deref())
}

fn effective_speed<'a>(
    options: &'a ModelOptions,
    details: Option<&'a ModelDetails>,
) -> Option<&'a str> {
    options
        .service_tier
        .as_deref()
        .or_else(|| details?.default_service_tier.as_deref())
}

fn model_snapshot(options: &ModelOptions, details: Option<&ModelDetails>) -> ModelOptions {
    let mut snapshot = options.clone();
    if let Some(details) = details {
        if snapshot.effort.is_none() {
            snapshot.effort = effective_effort(options, Some(details))
                .filter(|effort| details.efforts.iter().any(|supported| supported == effort))
                .map(str::to_owned);
        }
        if let Some(fast_tier) = &details.fast_tier {
            snapshot.service_tier = Some(
                if effective_speed(options, Some(details)).is_some_and(is_fast_tier) {
                    fast_tier.clone()
                } else {
                    STANDARD_SPEED.into()
                },
            );
        }
    }
    snapshot
}

impl Models {
    fn report<'a>(&self, cx: &'a App) -> Option<&'a ProviderReport> {
        self.catalog.as_ref()?.read(cx).report()
    }

    fn driver(&self, provider: &ProviderInstanceId) -> Option<Provider> {
        self.providers.get(provider).map(|(driver, _)| *driver)
    }

    fn provider_label<'a>(&'a self, provider: &'a ProviderInstanceId) -> &'a str {
        self.providers
            .get(provider)
            .map_or(provider.as_str(), |(_, label)| label)
    }

    fn favorite_report<'a>(
        &self,
        favorite: &ModelFavorite,
        cx: &'a App,
    ) -> Option<&'a ProviderReport> {
        self.catalogs.get(&favorite.provider)?.read(cx).report()
    }

    fn favorite_status(&self, favorite: &ModelFavorite, cx: &App) -> Option<&'static str> {
        if !self.allow_provider_switch && self.source != favorite.provider {
            return Some("New chat only");
        }
        if self
            .favorite_preferences
            .get(&favorite.provider)
            .is_some_and(|prefs| !prefs.enabled(&favorite.model))
        {
            return Some("Hidden in Settings");
        }
        if self
            .catalogs
            .get(&favorite.provider)
            .is_some_and(|catalog| catalog.read(cx).loading())
        {
            return Some("Checking…");
        }
        if !self
            .favorite_report(favorite, cx)
            .is_some_and(|report| favorite.supported_by(report))
        {
            return Some("Unavailable");
        }
        None
    }

    fn favorite_name(&self, favorite: &ModelFavorite, cx: &App) -> String {
        let report = self.favorite_report(favorite, cx);
        let details = model_details(report, Some(&favorite.model));
        display_name(
            self.driver(&favorite.provider),
            Some(&favorite.model),
            model_name(Some(&favorite.name), details),
        )
    }

    fn model_label(&self, model: &str, cx: &App) -> String {
        display_name(
            self.driver(&self.source),
            Some(model),
            model_name(Some(model), model_details(self.report(cx), Some(model))),
        )
    }

    fn filter(&mut self, cx: &App) {
        if self.favorites_open {
            self.matches = self
                .saved
                .iter()
                .filter(|favorite| {
                    let name = self.favorite_name(favorite, cx);
                    let provider = self.provider_label(&favorite.provider);
                    format!("{name} {} {} {provider}", favorite.name, favorite.model)
                        .to_lowercase()
                        .contains(&self.query)
                })
                .cloned()
                .map(ModelChoice::Favorite)
                .collect();
            return;
        }
        let report = self.report(cx);
        let models = report.map(|report| self.preferences.ordered(&report.models));
        let choices = models
            .into_iter()
            .flatten()
            .filter(|id| self.preferences.enabled(id))
            .filter(|id| {
                let name = model_name(Some(id), model_details(report, Some(id)));
                self.model_label(id, cx)
                    .to_lowercase()
                    .contains(&self.query)
                    || name.to_lowercase().contains(&self.query)
                    || id.to_lowercase().contains(&self.query)
            })
            .map(|id| ModelChoice::Model(id.to_owned()));
        self.matches = choices.collect();
    }
}

impl ListDelegate for Models {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matches.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let choice = match self.matches.get(ix.row)? {
            ModelChoice::Model(choice) => choice,
            ModelChoice::Favorite(favorite) => {
                let driver = self.driver(&favorite.provider);
                let report = self.favorite_report(favorite, cx);
                let details = model_details(report, Some(&favorite.model));
                let effort = effective_effort(&favorite.options, details).map(effort_label);
                let speed = effective_speed(&favorite.options, details)
                    .map(ModelFavorite::speed_label)
                    .or_else(|| details?.fast_tier.as_ref().map(|_| "Standard"));
                let follows_provider = details.is_some_and(|details| {
                    (favorite.options.effort.is_none() && details.default_effort.is_some())
                        || (favorite.options.service_tier.is_none() && details.fast_tier.is_some())
                });
                let summary = [effort.as_deref(), speed]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                let status = self.favorite_status(favorite, cx);
                let name = self.favorite_name(favorite, cx);
                let provider = self.provider_label(&favorite.provider).to_owned();
                let exact_favorite = self.saved.iter().any(|saved| {
                    saved.provider == favorite.provider
                        && saved.model == favorite.model
                        && saved.options == self.options
                });
                let current = status.is_none()
                    && self.source == favorite.provider
                    && self.current.as_deref() == Some(favorite.model.as_str())
                    && if exact_favorite {
                        self.options == favorite.options
                    } else {
                        model_snapshot(&self.options, details) == favorite.options
                    };
                #[cfg(all(feature = "qualification", target_os = "macos"))]
                let model = favorite.model.clone();
                let favorite = favorite.clone();
                let desktop = self.desktop.clone();
                #[cfg(all(feature = "qualification", target_os = "macos"))]
                let remove_key = format!(
                    "remove-model-favorite/{}/{model}/{summary}",
                    favorite.provider
                );
                return Some(
                    ListItem::new(ix.row)
                        .h(px(46.))
                        .rounded_md()
                        .text_sm()
                        .disabled(status.is_some())
                        .check_icon(IconName::Check)
                        .confirmed(current)
                        .child(control!(
                            format!(
                                "{}/{model}/{summary}",
                                if status.is_none() {
                                    "model-favorite"
                                } else {
                                    "unavailable-model-favorite"
                                }
                            ),
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(model_logo(driver, Some(&favorite.model), cx))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().truncate().child(name.clone()))
                                        .child(hint(provider, cx).truncate())
                                )
                                .child(
                                    v_flex()
                                        .items_end()
                                        .min_w_0()
                                        .when(!summary.is_empty(), |column| column
                                            .child(hint(summary, cx).truncate()))
                                        .when_some(
                                            status
                                                .or(follows_provider.then_some("Follows provider")),
                                            |column, status| column.child(hint(status, cx))
                                        )
                                )
                        ))
                        .suffix(move |_, _| {
                            control!(
                                remove_key.clone(),
                                Button::new("remove-model-favorite")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip(format!("Remove {name} from Favorites"))
                                    .on_click({
                                        let desktop = desktop.clone();
                                        let favorite = favorite.clone();
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            let _ = desktop.update(cx, |view, cx| {
                                                view.set_model_favorite(
                                                    favorite.clone(),
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    })
                            )
                        }),
                );
            }
        };
        let report = self.report(cx);
        let details = model_details(report, Some(choice));
        let driver = self.driver(&self.source);
        let name = self.model_label(choice, cx);
        #[cfg(all(feature = "qualification", target_os = "macos"))]
        let control_key = format!("model-option/{choice}");
        let selected = self
            .current
            .as_deref()
            .or_else(|| reported_default_model(report))
            == Some(choice);
        let effort = if selected {
            effective_effort(&self.options, details)
        } else {
            details.and_then(|details| details.default_effort.as_deref())
        };
        Some(
            ListItem::new(ix.row)
                .h(px(32.))
                .rounded_md()
                .text_sm()
                .check_icon(IconName::Check)
                .confirmed(selected)
                .child(control!(
                    control_key.clone(),
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(model_logo(driver, Some(choice), cx))
                        .child(
                            control!(
                                format!("{control_key}/name/{name}"),
                                div().truncate().child(name)
                            )
                            .min_w_0()
                        )
                        .when_some(effort, |row, effort| {
                            row.child(hint(effort_label(effort), cx).flex_shrink_0())
                        })
                )),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        hint(
            if !self.query.is_empty() {
                "No matching models"
            } else if self.favorites_open {
                "Save a model with the star below to find it here."
            } else if self.catalog.is_some() {
                "No models to show"
            } else {
                "Choose a provider to see its models."
            },
            cx,
        )
        .p_2()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.query = query.to_lowercase();
        self.filter(cx);
        Task::ready(())
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
    }
}

impl Desktop {
    pub(super) fn model_source(&self, cx: &App) -> (ProviderInstanceId, PathBuf) {
        match &self.current {
            Some(thread) => (
                thread.provider_instance.clone(),
                self.workspaces[&thread.id].root.clone(),
            ),
            None => (self.instance.clone(), self.launch_checkout(cx)),
        }
    }

    pub(super) fn preload_models(&mut self, cx: &mut Context<Self>) {
        let source = self.picker_source(cx);
        if !self.model_catalogs.contains_key(&source)
            && source.1.is_dir()
            && self
                .configured_providers(cx)
                .any(|provider| provider.id == source.0)
        {
            self.catalog_for(source, false, cx);
        }
    }

    fn picker_source(&self, cx: &App) -> (ProviderInstanceId, PathBuf) {
        let (instance, checkout) = self.model_source(cx);
        let checkout = if checkout.as_os_str().is_empty() {
            self.selection_file
                .parent()
                .expect("state directory")
                .to_owned()
        } else {
            checkout
        };
        (instance, checkout)
    }

    fn configured_providers<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = &'a huggler::ProviderInstance> {
        let (_, checkout) = self.picker_source(cx);
        self.runtime.provider_instances().filter(move |instance| {
            self.settings.read(cx).provider_configured(instance)
                && !self
                    .model_catalogs
                    .get(&(instance.id.clone(), checkout.clone()))
                    .is_some_and(|catalog| catalog.read(cx).login_required())
        })
    }

    pub(super) fn catalog(
        &mut self,
        refresh: bool,
        cx: &mut Context<Self>,
    ) -> Entity<ProviderCatalog> {
        self.catalog_for(self.model_source(cx), refresh, cx)
    }

    pub(super) fn catalog_for(
        &mut self,
        source: (ProviderInstanceId, PathBuf),
        refresh: bool,
        cx: &mut Context<Self>,
    ) -> Entity<ProviderCatalog> {
        if let Some(catalog) = self.model_catalogs.get(&source) {
            let state = catalog.read(cx);
            if !state.loading() && (refresh || state.login_required()) {
                catalog.update(cx, |state, cx| state.load(self.runtime.clone(), cx));
            }
            return catalog.clone();
        }
        let catalog = cx.new(|cx| {
            let mut catalog = ProviderCatalog {
                instance: source.0.clone(),
                checkout: source.1.clone(),
                result: None,
            };
            catalog.load(self.runtime.clone(), cx);
            catalog
        });
        cx.observe(&catalog, |_, _, cx| cx.notify()).detach();
        self.model_catalogs.insert(source, catalog.clone());
        cx.notify();
        catalog
    }

    fn model_selection(&self) -> (Option<&str>, &ModelOptions) {
        match &self.current {
            Some(thread) => (thread.model.as_deref(), &thread.model_options),
            None => (self.launch_model.as_deref(), &self.launch_model_options),
        }
    }

    fn update_model_options(
        &mut self,
        update: impl FnOnce(&mut ModelOptions),
        cx: &mut Context<Self>,
    ) {
        let (model, options) = self.model_selection();
        let mut options = options.clone();
        update(&mut options);
        self.set_model_selection(model.map(str::to_owned), options, cx);
    }

    fn set_model_selection(
        &mut self,
        model: Option<String>,
        options: ModelOptions,
        cx: &mut Context<Self>,
    ) {
        if let Some(thread) = &self.current {
            let result = self
                .runtime
                .dispatch(Command {
                    id: CommandId(Uuid::new_v4().to_string()),
                    thread_id: thread.id.clone(),
                    action: Action::SetModelSelection { model, options },
                })
                .and_then(|_| self.refresh());
            self.report(result, cx);
        } else {
            self.launch_model = model;
            self.launch_model_options = options;
            cx.notify();
        }
        if let Some(list) = &self.model_picker {
            let options = self.model_selection().1.clone();
            list.update(cx, |list, cx| {
                list.delegate_mut().options = options;
                cx.notify();
            });
        }
    }

    fn close_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_picker.take().is_some() {
            self.focus_workspace(window, cx);
            cx.notify();
        }
    }

    fn open_models(&mut self, favorites_open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.picker_source(cx);
        let thread_id = self.current.as_ref().map(|thread| thread.id.clone());
        let configured: HashSet<_> = self
            .configured_providers(cx)
            .map(|provider| provider.id.clone())
            .collect();
        let providers = self
            .runtime
            .provider_instances()
            .map(|provider| {
                (
                    provider.id.clone(),
                    (
                        provider.driver,
                        self.runtime
                            .output()
                            .provider_label(provider.id.as_str())
                            .to_owned(),
                    ),
                )
            })
            .collect();
        let catalog = configured
            .contains(&source.0)
            .then(|| self.catalog_for(source.clone(), false, cx));
        let (current, options) = self.model_selection();
        let current = current.map(str::to_owned);
        let options = options.clone();
        let settings = self.settings.read(cx);
        let saved = if favorites_open {
            settings.model_favorites().to_vec()
        } else {
            Vec::new()
        };
        let preferences = settings.model_preferences(&source.0);
        let favorite_preferences: HashMap<_, _> = saved
            .iter()
            .map(|favorite| {
                (
                    favorite.provider.clone(),
                    settings.model_preferences(&favorite.provider),
                )
            })
            .collect();
        let checkable: HashSet<_> = saved
            .iter()
            .filter(|favorite| {
                (thread_id.is_none() || favorite.provider == source.0)
                    && configured.contains(&favorite.provider)
                    && favorite_preferences[&favorite.provider].enabled(&favorite.model)
            })
            .map(|favorite| favorite.provider.clone())
            .collect();
        let catalogs = checkable
            .into_iter()
            .map(|provider| {
                let catalog = self.catalog_for((provider.clone(), source.1.clone()), false, cx);
                (provider, catalog)
            })
            .collect();
        let desktop = cx.weak_entity();
        let list = cx.new(|cx| {
            let mut models = Models {
                source: source.0.clone(),
                desktop,
                providers,
                allow_provider_switch: thread_id.is_none(),
                catalog: catalog.clone(),
                current,
                options,
                favorites_open,
                reasoning_menu: None,
                saved,
                favorite_preferences,
                catalogs,
                query: String::new(),
                matches: Vec::new(),
                preferences,
            };
            models.filter(cx);
            let catalogs = catalog.iter().chain(
                models
                    .catalogs
                    .values()
                    .filter(|candidate| Some(*candidate) != catalog.as_ref()),
            );
            for catalog in catalogs {
                cx.observe_in(
                    catalog,
                    window,
                    |list: &mut ListState<Models>, _, window, cx| {
                        let selected = list.selected_index();
                        let models = list.delegate_mut();
                        let choice = selected
                            .and_then(|index| models.matches.get(index.row))
                            .cloned();
                        models.filter(cx);
                        if let Some(index) = selected
                            && models.matches.get(index.row) != choice.as_ref()
                        {
                            list.set_selected_index(None, window, cx);
                        }
                        cx.notify();
                    },
                )
                .detach();
            }
            ListState::new(models, window, cx).searchable(true)
        });
        cx.subscribe_in(&list, window, move |view, list, event, window, cx| {
            if view.model_picker.as_ref() != Some(list) {
                return;
            }
            if matches!(event, ListEvent::Cancel) {
                view.close_model_picker(window, cx);
                return;
            }
            let ListEvent::Confirm(ix) = event else {
                return;
            };
            let models = list.read(cx).delegate();
            let Some(choice) = models.matches.get(ix.row).cloned() else {
                return;
            };
            if let ModelChoice::Favorite(favorite) = &choice
                && models.favorite_status(favorite, cx).is_some()
            {
                return;
            }
            let same_context = view.picker_source(cx) == source
                && view.current.as_ref().map(|t| &t.id) == thread_id.as_ref();
            let provider_model = reported_default_model(models.report(cx)).map(str::to_owned);
            view.close_model_picker(window, cx);
            if !same_context {
                return;
            }
            match choice {
                ModelChoice::Favorite(favorite) => {
                    if thread_id.is_none() {
                        view.choose_provider(&favorite.provider, cx);
                    }
                    view.set_model_selection(Some(favorite.model), favorite.options, cx);
                }
                ModelChoice::Model(model) if view.model_selection().0 != Some(model.as_str()) => {
                    let options = if provider_model.as_deref() == Some(model.as_str())
                        && view.model_selection().0.is_none()
                    {
                        view.model_selection().1.clone()
                    } else {
                        ModelOptions::default()
                    };
                    view.set_model_selection(Some(model), options, cx);
                }
                ModelChoice::Model(_) => {}
            }
        })
        .detach();
        list.focus_handle(cx).focus(window);
        self.model_picker = Some(list);
        cx.notify();
    }
}
