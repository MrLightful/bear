use super::*;
use huggler::{ModelOptions, ProviderReport};

pub(crate) const STANDARD_SPEED: &str = "default";

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ModelFavorite {
    pub provider: ProviderInstanceId,
    pub model: String,
    pub name: String,
    pub options: ModelOptions,
}

impl ModelFavorite {
    pub(crate) fn same_selection(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.model == other.model
            && self.options == other.options
    }

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

    pub(crate) fn speed_label(tier: &str) -> &'static str {
        match tier {
            STANDARD_SPEED => "Standard",
            tier if is_fast_tier(tier) => "Fast",
            _ => "Unavailable",
        }
    }
}

pub(crate) fn is_fast_tier(tier: &str) -> bool {
    matches!(tier, "fast" | "priority")
}

impl Settings {
    pub(crate) fn model_favorites(&self) -> &[ModelFavorite] {
        &self.prefs.model_favorites
    }

    pub(crate) fn set_model_favorite(
        &mut self,
        favorite: ModelFavorite,
        saved: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut prefs = self.prefs.clone();
        let favorites = &mut prefs.model_favorites;
        match (
            saved,
            favorites
                .iter()
                .position(|saved| saved.same_selection(&favorite)),
        ) {
            (true, None) => favorites.push(favorite),
            (false, Some(index)) => {
                favorites.remove(index);
            }
            _ => return true,
        }
        self.persist(prefs, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::ModelFavorite;
    use huggler::{ModelDetails, ModelOptions, ProviderReport};
    use std::collections::BTreeMap;

    #[test]
    fn a_favorite_requires_the_saved_model_and_options() {
        let mut preset = ModelFavorite {
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
