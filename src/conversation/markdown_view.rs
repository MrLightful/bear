mod mermaid;
mod response_image;

use super::{
    Desktop,
    document_tabs::{Document, open_workspace_file},
    transcript::transcript_muted,
};
use anyhow::{Context as _, Result, ensure};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme, Icon, IconName, h_flex,
    text::{LinkHandler, TextView, TextViewStyle},
    v_flex,
};
use markdown::mdast::Node;
use percent_encoding::percent_decode_str;
use std::{
    borrow::Cow,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub(crate) fn prose(
    id: String,
    source: &str,
    response: ResponseRenderContext<'_>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    Markdown::parse(source.to_owned()).render_blocks(id, source, Some(&response), None, window, cx)
}

pub(crate) fn contains_mermaid_fence(source: &str) -> bool {
    source.lines().any(|line| {
        ["```", "~~~"].iter().any(|marker| {
            line.split_once(marker).is_some_and(|(_, info)| {
                info.trim_start_matches(['`', '~'])
                    .split_whitespace()
                    .next()
                    .is_some_and(mermaid::is_language)
            })
        })
    })
}

pub(crate) struct ResponseRenderContext<'a> {
    pub(crate) run: &'a huggler::RunId,
    pub(crate) checkout: Option<&'a Path>,
    pub(crate) desktop: Option<WeakEntity<Desktop>>,
    pub(crate) executor: &'a tokio::runtime::Handle,
    pub(crate) scroll: &'a ScrollHandle,
}

pub(super) struct Markdown {
    source: String,
    root: Option<markdown::mdast::Root>,
    definitions: String,
}

impl Markdown {
    pub(super) fn new(source: &str) -> Self {
        Self::parse(safe_markdown(source, false, false))
    }

    fn parse(source: String) -> Self {
        let root = match markdown::to_mdast(&source, &markdown::ParseOptions::gfm()) {
            Ok(Node::Root(root)) => Some(root),
            _ => None,
        };
        let definitions = root
            .iter()
            .flat_map(|root| &root.children)
            .filter(|node| matches!(node, Node::Definition(_)))
            .filter_map(|node| node_source(node, &source))
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            source,
            root,
            definitions,
        }
    }

    pub(super) fn render(
        &self,
        id: String,
        raw: &str,
        heading: Option<(&str, &ScrollHandle)>,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        self.render_blocks(id, raw, None, heading, window, cx)
    }

    fn render_blocks(
        &self,
        id: String,
        raw: &str,
        response: Option<&ResponseRenderContext<'_>>,
        heading: Option<(&str, &ScrollHandle)>,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let document = response.is_none();
        let mut body = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .text_sm()
            .line_height(relative(1.55))
            .when(document, |body| body.gap_3().line_height(relative(1.65)));
        let Some(root) = &self.root else {
            return body.child(raw.to_owned());
        };
        let on_link_click = response.and_then(workspace_link_handler);
        let anchor = heading.and_then(|(fragment, scroll)| {
            Some((
                heading_index(root, fragment)?,
                ScrollAnchor::for_handle(scroll.clone()),
            ))
        });
        let blocks = MarkdownBlock {
            source: &self.source,
            definitions: &self.definitions,
            in_list: false,
            quote_depth: 0,
            document,
            images: response,
            on_link_click: on_link_click.as_ref(),
            root,
        };
        for (index, node) in root.children.iter().enumerate() {
            if let Some(block) = blocks.render(node, &format!("{id}-{index}"), window, cx) {
                body = match anchor.as_ref().filter(|(target, _)| index == *target) {
                    Some((_, anchor)) => body.child(
                        div()
                            .id("markdown-heading-anchor")
                            .anchor_scroll(Some(anchor.clone()))
                            .child(block),
                    ),
                    None => body.child(block),
                };
            }
        }
        if let Some((_, anchor)) = anchor {
            anchor.scroll_to(window, cx);
            window.on_next_frame(|window, _| window.refresh());
        }
        body
    }
}

fn heading_index(root: &markdown::mdast::Root, fragment: &str) -> Option<usize> {
    let mut slugs = std::collections::HashSet::new();
    root.children.iter().enumerate().find_map(|(index, node)| {
        if !matches!(node, Node::Heading(_)) {
            return None;
        }
        let base: String = node
            .to_string()
            .chars()
            .flat_map(char::to_lowercase)
            .filter_map(|c| match c {
                ' ' => Some('-'),
                '-' | '_' => Some(c),
                c if c.is_alphanumeric() => Some(c),
                _ => None,
            })
            .collect();
        let mut slug = base.clone();
        for suffix in 1.. {
            if slugs.insert(slug.clone()) {
                break;
            }
            slug = format!("{base}-{suffix}");
        }
        (slug == fragment).then_some(index)
    })
}

struct MarkdownBlock<'a> {
    source: &'a str,
    definitions: &'a str,
    in_list: bool,
    quote_depth: usize,
    document: bool,
    images: Option<&'a ResponseRenderContext<'a>>,
    on_link_click: Option<&'a LinkHandler>,
    root: &'a markdown::mdast::Root,
}

impl MarkdownBlock<'_> {
    fn render(&self, node: &Node, id: &str, window: &mut Window, cx: &mut App) -> Option<Div> {
        match node {
            // GPUI Component's intrinsic list layout can under-measure wrapped
            // rows. Give each marker and its contents separate flex columns.
            Node::List(list) => Some(
                v_flex()
                    .gap_1()
                    .children(
                        list.children
                            .iter()
                            .enumerate()
                            .filter_map(|(index, node)| {
                                let Node::ListItem(item) = node else {
                                    return None;
                                };
                                let marker = match list.start {
                                    Some(start) => {
                                        format!("{}.", u64::from(start) + index as u64)
                                    }
                                    _ => "•".into(),
                                };
                                let children = Self {
                                    in_list: true,
                                    ..*self
                                }
                                .children(
                                    &item.children,
                                    &format!("{id}-{index}"),
                                    window,
                                    cx,
                                );
                                Some(
                                    h_flex()
                                        .items_start()
                                        .gap_2()
                                        .child(
                                            div()
                                                .min_w(px(14.))
                                                .text_right()
                                                .text_color(transcript_muted(cx))
                                                .children(item.checked.is_none().then_some(marker))
                                                .children(item.checked.map(|checked| {
                                                    div()
                                                        .mt_1()
                                                        .size_3()
                                                        .border_1()
                                                        .rounded_sm()
                                                        .border_color(cx.theme().border)
                                                        .when(checked, |div| {
                                                            div.child(
                                                                Icon::new(IconName::Check).size_3(),
                                                            )
                                                        })
                                                })),
                                        )
                                        .child(children.flex_1().min_w_0()),
                                )
                            }),
                    ),
            ),
            Node::Definition(_) => None,
            Node::Code(code)
                if code.lang.as_deref().is_some_and(mermaid::is_language)
                    && node_source(node, self.source).is_some_and(mermaid::is_closed) =>
            {
                let Some(images) = self.images else {
                    return self.text_block(node, id, window, cx);
                };
                Some(mermaid::render(
                    id,
                    &code.value,
                    |window, cx| self.text_block(node, id, window, cx),
                    images.scroll,
                    window,
                    cx,
                ))
            }
            Node::Paragraph(paragraph) if self.images.is_some() => self
                .response_paragraph(node, &paragraph.children, id, window, cx)
                .or_else(|| self.text_block(node, id, window, cx)),
            Node::Blockquote(quote) if self.images.is_some() && contains_custom_block(node) => {
                Some(
                    Self {
                        quote_depth: self.quote_depth + 1,
                        ..*self
                    }
                    .children(&quote.children, id, window, cx)
                    .pl_3()
                    .border_l_2()
                    .border_color(cx.theme().border),
                )
            }
            _ => self.text_block(node, id, window, cx),
        }
    }

    fn text_block(&self, node: &Node, id: &str, window: &mut Window, cx: &mut App) -> Option<Div> {
        let position = node.position()?;
        let source = node_source(node, self.source)?;
        Some(self.text(
            source,
            position.start.column.saturating_sub(1),
            matches!(node, Node::Heading(_)),
            id,
            window,
            cx,
        ))
    }

    fn text(
        &self,
        source: &str,
        prefix_width: usize,
        heading: bool,
        id: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let source = standalone_source(source, prefix_width, self.in_list, self.quote_depth);
        let source = if self.definitions.is_empty() {
            source.into_owned()
        } else {
            format!("{source}\n\n{}", self.definitions)
        };
        let source = if self.images.is_some() {
            safe_markdown(&source, true, self.on_link_click.is_some())
        } else {
            source
        };
        let style = TextViewStyle {
            highlight_theme: cx.theme().highlight_theme.clone(),
            is_dark: cx.theme().mode.is_dark(),
            ..Default::default()
        }
        .heading_font_size({
            let document = self.document;
            move |level, _| {
                px(match (document, level) {
                    (true, 1) => 28.,
                    (true, 2) => 21.,
                    (true, 3) => 17.,
                    (_, 1) => 20.,
                    (_, 2) => 18.,
                    _ => 15.,
                })
            }
        });
        div()
            .w_full()
            .min_w_0()
            .when(self.document && heading, |block| block.mt_3())
            .child(
                TextView::markdown(SharedString::from(id.to_owned()), source, window, cx)
                    .style(style)
                    .when_some(self.on_link_click.cloned(), TextView::on_link_click)
                    .selectable(true),
            )
    }

    fn response_paragraph(
        &self,
        node: &Node,
        children: &[Node],
        id: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Div> {
        let images = self.images?;
        let position = node.position()?;
        let prefix = position.start.column.saturating_sub(1);
        let mut start = position.start.offset;
        let mut body = v_flex().w_full().min_w_0().gap_2();
        for (index, child) in children.iter().enumerate() {
            let Some(image) = inline_image(child, self.root) else {
                continue;
            };
            let span = child.position()?;
            let before = &self.source[start..span.start.offset];
            if !before.trim().is_empty() {
                body = body.child(self.text(
                    before,
                    prefix,
                    false,
                    &format!("{id}-text-{index}"),
                    window,
                    cx,
                ));
            }
            body = body.child(response_image::render(
                &format!("{id}-image-{index}"),
                image,
                images,
                window,
                cx,
            ));
            start = span.end.offset;
        }
        if start == position.start.offset {
            return None;
        }
        let after = &self.source[start..position.end.offset];
        if !after.trim().is_empty() {
            body = body.child(self.text(after, prefix, false, &format!("{id}-end"), window, cx));
        }
        Some(body)
    }

    fn children(&self, nodes: &[Node], id: &str, window: &mut Window, cx: &mut App) -> Div {
        v_flex().gap_1().children(
            nodes.iter().enumerate().filter_map(|(index, node)| {
                self.render(node, &format!("{id}-{index}"), window, cx)
            }),
        )
    }
}

struct InlineImage<'a> {
    destination: &'a str,
    alt: &'a str,
    link: Option<&'a str>,
}

fn definition_url<'a>(nodes: &'a [Node], identifier: &str) -> Option<&'a str> {
    nodes.iter().find_map(|node| match node {
        Node::Definition(definition) if definition.identifier == identifier => {
            Some(definition.url.as_str())
        }
        _ => definition_url(node.children()?, identifier),
    })
}

fn inline_image<'a>(node: &'a Node, root: &'a markdown::mdast::Root) -> Option<InlineImage<'a>> {
    match node {
        Node::Image(image) => Some(InlineImage {
            destination: &image.url,
            alt: &image.alt,
            link: None,
        }),
        Node::ImageReference(image) => Some(InlineImage {
            destination: definition_url(&root.children, &image.identifier)?,
            alt: &image.alt,
            link: None,
        }),
        Node::Link(_)
        | Node::LinkReference(_)
        | Node::Emphasis(_)
        | Node::Strong(_)
        | Node::Delete(_) => match node.children()?.as_slice() {
            [child] => {
                let mut image = inline_image(child, root)?;
                let link = match node {
                    Node::Link(link) => Some(link.url.as_str()),
                    Node::LinkReference(link) => definition_url(&root.children, &link.identifier),
                    _ => image.link,
                };
                image.link = link.filter(|url| web_url(url));
                Some(image)
            }
            _ => None,
        },
        _ => None,
    }
}

fn contains_custom_block(node: &Node) -> bool {
    matches!(node, Node::Image(_) | Node::ImageReference(_))
        || matches!(node, Node::Code(code) if code.lang.as_deref().is_some_and(mermaid::is_language))
        || node
            .children()
            .is_some_and(|children| children.iter().any(contains_custom_block))
}

fn standalone_source(
    source: &str,
    prefix_width: usize,
    in_list: bool,
    quote_depth: usize,
) -> Cow<'_, str> {
    if (!in_list || prefix_width == 0) && quote_depth == 0 {
        return source.into();
    }
    let Some((first, rest)) = source.split_once('\n') else {
        return source.into();
    };
    // The first line starts at the block's marker. Continuation lines still
    // carry the list indentation in the original source.
    let mut standalone = String::with_capacity(source.len());
    standalone.push_str(first);
    standalone.push('\n');
    for line in rest.split_inclusive('\n') {
        let mut content = line;
        for _ in 0..quote_depth {
            let Some(quote) = content.trim_start_matches(' ').strip_prefix('>') else {
                break;
            };
            content = quote.strip_prefix(' ').unwrap_or(quote);
        }
        let prefix = content
            .bytes()
            .take(if in_list {
                prefix_width.saturating_sub(line.len() - content.len())
            } else {
                0
            })
            .take_while(|byte| *byte == b' ')
            .count();
        standalone.push_str(&content[prefix..]);
    }
    standalone.into()
}

fn node_source<'a>(node: &Node, source: &'a str) -> Option<&'a str> {
    let pos = node.position()?;
    source.get(pos.start.offset..pos.end.offset)
}

// GPUI's renderer can load images and open links. Keep raw HTML and image URLs
// inert. Workspace file links are only enabled when a scoped handler is installed.
fn safe_markdown(source: &str, omit_unsafe_definitions: bool, workspace_links: bool) -> String {
    fn visit(
        node: &Node,
        source: &str,
        omit_unsafe_definitions: bool,
        workspace_links: bool,
        edits: &mut Vec<(std::ops::Range<usize>, String)>,
    ) {
        let allowed_link =
            |url: &str| web_url(url) || (workspace_links && workspace_link_path(url).is_some());
        let replacement = match node {
            Node::Image(image) => Some(escape_markdown(&image.alt)),
            Node::ImageReference(image) => Some(escape_markdown(&image.alt)),
            Node::Html(_) => node
                .position()
                .map(|pos| escape_markdown(&source[pos.start.offset..pos.end.offset])),
            Node::Link(link) if !allowed_link(&link.url) => node
                .position()
                .map(|pos| escape_markdown(&source[pos.start.offset..pos.end.offset])),
            Node::Definition(link) if !allowed_link(&link.url) && omit_unsafe_definitions => {
                Some(String::new())
            }
            Node::Definition(link) if !allowed_link(&link.url) => node
                .position()
                .map(|pos| escape_markdown(&source[pos.start.offset..pos.end.offset])),
            _ => None,
        };
        if let Some(replacement) = replacement
            && let Some(pos) = node.position()
        {
            edits.push((pos.start.offset..pos.end.offset, replacement));
        } else if let Some(children) = node.children() {
            for child in children {
                visit(
                    child,
                    source,
                    omit_unsafe_definitions,
                    workspace_links,
                    edits,
                );
            }
        }
    }
    let mut safe = source.to_owned();
    if source.contains(['[', '<'])
        && let Ok(tree) = markdown::to_mdast(source, &markdown::ParseOptions::gfm())
    {
        let mut edits = Vec::new();
        visit(
            &tree,
            source,
            omit_unsafe_definitions,
            workspace_links,
            &mut edits,
        );
        for (span, text) in edits.into_iter().rev() {
            safe.replace_range(span, &text);
        }
    }
    safe
}

fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_punctuation() {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn workspace_link_handler(response: &ResponseRenderContext<'_>) -> Option<LinkHandler> {
    let desktop = response.desktop.clone()?;
    let checkout = response.checkout?.to_owned();
    Some(Arc::new(move |url, window, cx| {
        if web_url(url) {
            cx.open_url(url);
            return;
        }
        let Some(path) = workspace_link_path(url) else {
            return;
        };
        let heading = url
            .split_once('#')
            .and_then(|(_, fragment)| {
                percent_decode_str(fragment)
                    .decode_utf8()
                    .ok()
                    .map(Cow::into_owned)
            })
            .filter(|fragment| !fragment.is_empty());
        let _ = desktop.update(cx, |desktop, cx| {
            let Some(thread) = desktop.current.as_ref().map(|thread| thread.id.clone()) else {
                return;
            };
            if desktop.workspaces[&thread].root != checkout {
                return;
            }
            let root = checkout.clone();
            let job = cx.background_spawn(async move {
                resolve_workspace_link(&root, &path).map(Path::to_path_buf)
            });
            let checkout = checkout.clone();
            cx.spawn_in(window, async move |view, cx| {
                let Some(relative) = job.await else {
                    return;
                };
                let _ = view.update_in(cx, |desktop, window, cx| {
                    if desktop
                        .current
                        .as_ref()
                        .is_some_and(|current| current.id == thread)
                        && desktop.workspaces[&thread].root == checkout
                    {
                        desktop.open_document_at_heading(
                            Document::File(relative.into()),
                            heading,
                            window,
                            cx,
                        );
                    }
                });
            })
            .detach();
        });
    }))
}

fn checkout_relative<'a>(checkout: &Path, path: &'a Path) -> Result<&'a Path> {
    if !path.is_absolute() {
        return Ok(path.strip_prefix(".").unwrap_or(path));
    }
    let canonical = checkout.canonicalize()?;
    if let Ok(relative) = path
        .strip_prefix(checkout)
        .or_else(|_| path.strip_prefix(&canonical))
    {
        return Ok(relative);
    }
    // Resolving a Windows UNC path can authenticate to a network share before
    // its scope is established. Only try alternate root spellings on Unix.
    ensure!(cfg!(unix), "Path is outside the checkout");
    // Accept aliases of the checkout's parent directories (for example /var
    // on macOS). The scoped reader still checks every component below the root.
    let root = path
        .ancestors()
        .skip(1)
        .find(|parent| {
            parent.file_name() == canonical.file_name()
                && parent
                    .canonicalize()
                    .is_ok_and(|parent| parent == canonical)
        })
        .context("Path is outside the checkout")?;
    Ok(path.strip_prefix(root)?)
}

fn resolve_workspace_link<'a>(checkout: &Path, path: &'a Path) -> Option<&'a Path> {
    let relative = checkout_relative(checkout, path).ok()?;
    let file = open_workspace_file(checkout, relative).ok()?;
    file.metadata().ok()?.is_file().then_some(relative)
}

fn workspace_link_path(value: &str) -> Option<PathBuf> {
    let path = value.split(['?', '#']).next()?;
    let path = path
        .rsplit_once(':')
        .filter(|(_, line)| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(path, |(path, _)| path);
    let path = percent_decode_str(path).decode_utf8().ok()?;
    if path.starts_with("//") || path.contains([':', '\\', '\0']) {
        return None;
    }
    let path = PathBuf::from(path.as_ref());
    if path.file_name().is_none()
        || path.components().any(|component| {
            matches!(component, Component::ParentDir | Component::Prefix(_))
                || matches!(component, Component::Normal(name) if name.eq_ignore_ascii_case(".git"))
        })
    {
        return None;
    }
    Some(path)
}

fn web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

#[cfg(test)]
mod tests {
    use super::{
        heading_index, node_source, resolve_workspace_link, safe_markdown, standalone_source,
        workspace_link_path,
    };
    use markdown::{ParseOptions, mdast::Node};
    use std::path::Path;

    #[test]
    fn response_markdown_preserves_workspace_links() {
        let source = concat!(
            "Added [Release Please](/checkout/.github/workflows/release-please.yml) ",
            "and a [changelog][changes]. [Guide](</checkout/docs/Release Guide.md#publish>)\n\n",
            "[changes]: /checkout/CHANGELOG.md:12",
        );
        let sanitized = safe_markdown(source, true, true);
        let tree = markdown::to_mdast(&sanitized, &ParseOptions::gfm()).unwrap();
        let paragraph = &tree.children().unwrap()[0];
        let children = paragraph.children().unwrap();
        assert!(
            matches!(&children[1], Node::Link(link) if link.url.ends_with("release-please.yml"))
        );
        assert!(matches!(&children[3], Node::LinkReference(link) if link.identifier == "changes"));
        assert!(
            matches!(&children[5], Node::Link(link) if link.url.ends_with("Release Guide.md#publish"))
        );
        let Node::Definition(definition) = &tree.children().unwrap()[1] else {
            panic!("expected the workspace link definition");
        };
        assert_eq!(
            workspace_link_path(&definition.url).as_deref(),
            Some(Path::new("/checkout/CHANGELOG.md"))
        );
        assert_eq!(
            workspace_link_path("./docs/Release%20Guide.md#publish").as_deref(),
            Some(Path::new("./docs/Release Guide.md"))
        );
        assert_ne!(safe_markdown(source, true, false), sanitized);
    }

    #[test]
    fn response_markdown_keeps_unsafe_destinations_inert() {
        for target in [
            "javascript:alert(1)",
            "file:///etc/hosts",
            "//host/share",
            "docs/%2e%2e/secret",
            ".git/config",
        ] {
            let source = format!("[open](<{target}>)");
            let sanitized = safe_markdown(&source, true, true);
            let tree = markdown::to_mdast(&sanitized, &ParseOptions::gfm()).unwrap();
            let children = tree.children().unwrap()[0].children().unwrap();
            assert!(children.iter().all(|node| matches!(node, Node::Text(_))));
        }
        let web = "[release](https://example.com/release)";
        assert_eq!(safe_markdown(web, true, true), web);
    }

    #[test]
    fn heading_fragments_match_formatted_and_duplicate_headings() {
        let Node::Root(root) = markdown::to_mdast(
            "# Release guide\n\n## Publish the `Rust` crate!\n\n## Publish the Rust crate",
            &ParseOptions::gfm(),
        )
        .unwrap() else {
            panic!("expected a Markdown root");
        };
        assert_eq!(heading_index(&root, "publish-the-rust-crate"), Some(1));
        assert_eq!(heading_index(&root, "publish-the-rust-crate-1"), Some(2));
        assert_eq!(heading_index(&root, "absent"), None);
    }

    #[test]
    fn workspace_links_resolve_only_to_regular_checkout_files() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let file = checkout.join("README.md");
        let outside = dir.path().join("outside.md");
        std::fs::write(&file, "example").unwrap();
        std::fs::write(&outside, "outside").unwrap();

        assert_eq!(
            resolve_workspace_link(&checkout, &file),
            Some(Path::new("README.md"))
        );
        assert!(resolve_workspace_link(&checkout, &outside).is_none());
        assert!(resolve_workspace_link(&checkout, Path::new("missing.md")).is_none());

        #[cfg(unix)]
        {
            let alias = checkout.join("alias.md");
            std::os::unix::fs::symlink(outside, &alias).unwrap();
            assert!(resolve_workspace_link(&checkout, &alias).is_none());
        }
    }

    #[test]
    fn standalone_list_blocks_keep_quotes_and_code_indentation() {
        let source = "1. > First line\n   > Second line\n\n   ```text\n     two spaces\n   ```";
        let tree = markdown::to_mdast(source, &markdown::ParseOptions::gfm()).unwrap();
        let list = &tree.children().unwrap()[0];
        let item = &list.children().unwrap()[0];
        let blocks = item.children().unwrap();
        assert_eq!(
            standalone_source(node_source(&blocks[0], source).unwrap(), 3, true, 0),
            "> First line\n> Second line"
        );
        assert_eq!(
            standalone_source(node_source(&blocks[1], source).unwrap(), 3, true, 0),
            "```text\n  two spaces\n```"
        );
    }
}
