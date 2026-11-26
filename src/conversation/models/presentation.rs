use super::{App, Provider, provider_logo};
use gpui::{AnyElement, IntoElement, Styled, px};
use gpui_component::{Icon, IconName};

fn model_provider(driver: Option<Provider>, model: Option<&str>) -> Option<Provider> {
    let namespaced = model.and_then(|id| id.split_once('/'));
    match namespaced {
        Some(("openai" | "openai-codex", _)) => Some(Provider::Codex),
        Some(("anthropic", _)) => Some(Provider::ClaudeAgent),
        Some(("google" | "google-vertex" | "google-gemini", _)) => Some(Provider::Antigravity),
        Some(("xai" | "x-ai", _)) => Some(Provider::Grok),
        Some(("meta" | "meta-llama", _)) => Some(Provider::Muse),
        _ => match model {
            Some(id) if id.starts_with("gpt-") => Some(Provider::Codex),
            Some(id) if id.starts_with("claude-") => Some(Provider::ClaudeAgent),
            Some(id) if id.starts_with("gemini-") => Some(Provider::Antigravity),
            Some(id) if id.starts_with("grok-") => Some(Provider::Grok),
            Some(id) if id.starts_with("llama-") => Some(Provider::Muse),
            _ if matches!(driver, Some(Provider::OpenCode | Provider::Pi))
                && namespaced.is_some() =>
            {
                None
            }
            _ => driver,
        },
    }
}

pub(super) fn display_name(driver: Option<Provider>, model: Option<&str>, name: &str) -> String {
    let provider = model_provider(driver, model);
    let name = match model.and_then(|id| id.split_once('/')) {
        Some((namespace, _))
            if matches!(
                provider,
                Some(
                    Provider::Codex
                        | Provider::ClaudeAgent
                        | Provider::Antigravity
                        | Provider::Grok
                        | Provider::Muse
                )
            ) =>
        {
            name.strip_prefix(namespace)
                .and_then(|name| name.strip_prefix('/'))
                .unwrap_or(name)
        }
        _ => name,
    };
    let lower = name.to_ascii_lowercase();
    let formatted = match provider {
        Some(Provider::Codex) => lower
            .strip_prefix("gpt-")
            .or_else(|| lower.strip_prefix("gpt "))
            .filter(|version| version.starts_with(|c: char| c.is_ascii_digit()))
            .map(title_model_slug),
        Some(Provider::ClaudeAgent) => lower.strip_prefix("claude-").map(claude_name),
        Some(Provider::Grok) if lower.starts_with("grok-") => Some(title_model_slug(&lower)),
        Some(Provider::Antigravity) if lower.starts_with("gemini-") => {
            Some(title_model_slug(&lower))
        }
        Some(Provider::Muse) if lower.starts_with("llama-") => Some(title_model_slug(&lower)),
        _ => None,
    };
    formatted.unwrap_or_else(|| name.to_owned())
}

fn claude_name(slug: &str) -> String {
    let parts: Vec<_> = slug.split('-').collect();
    match parts.as_slice() {
        [
            major,
            minor,
            family @ ("opus" | "sonnet" | "haiku"),
            suffix @ ..,
        ]
        | [
            family @ ("opus" | "sonnet" | "haiku"),
            major,
            minor,
            suffix @ ..,
        ] if is_version_digit(major) && is_version_digit(minor) => title_model_slug(&format!(
            "claude-{family}-{major}.{minor}-{}",
            suffix.join("-")
        )),
        _ => title_model_slug(&format!("claude-{slug}")),
    }
}

fn is_version_digit(part: &str) -> bool {
    part.len() == 1 && part.starts_with(|c: char| c.is_ascii_digit())
}

fn title_model_slug(slug: &str) -> String {
    slug.split(['-', ' '])
        .filter(|part| !part.is_empty())
        .map(super::effort_label)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn model_logo(driver: Option<Provider>, model: Option<&str>, cx: &App) -> AnyElement {
    match model_provider(driver, model) {
        Some(provider) => provider_logo(provider, px(16.), cx).into_any_element(),
        None => Icon::new(IconName::Bot)
            .size(px(16.))
            .flex_shrink_0()
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_follow_the_model_provider() {
        for (driver, id, name, expected) in [
            (Provider::Codex, "gpt-6.2-sol", "GPT-6.2-Sol", "6.2 Sol"),
            (
                Provider::Pi,
                "anthropic/claude-opus-4-6",
                "anthropic/claude-opus-4-6",
                "Claude Opus 4.6",
            ),
            (
                Provider::ClaudeAgent,
                "claude-3-7-sonnet",
                "claude-3-7-sonnet",
                "Claude Sonnet 3.7",
            ),
            (Provider::Cursor, "grok-build", "grok-build", "Grok Build"),
            (
                Provider::OpenCode,
                "google/gemini-3.1-pro",
                "gemini-3.1-pro",
                "Gemini 3.1 Pro",
            ),
            (
                Provider::Pi,
                "custom/my-model",
                "custom/my-model",
                "custom/my-model",
            ),
        ] {
            assert_eq!(display_name(Some(driver), Some(id), name), expected);
        }
    }
}
