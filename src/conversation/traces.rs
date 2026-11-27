use super::*;
use crate::ui::keyboard_scroll_viewport;
use gpui_component::scroll::ScrollableElement;
use huggler::{ActivityKind, ActivityStatus, ToolTrace, ToolTraceSource};

const PAGE_SIZE: usize = 50;
const ROW_HEIGHT: Pixels = px(42.);
const LIST_MARGIN: Pixels = px(4.);
const FAILURE_DOT: Pixels = px(6.);
const FILTERS: [(Option<ToolTraceSource>, &str); 3] = [
    (None, "All"),
    (Some(ToolTraceSource::Mcp), "MCP"),
    (Some(ToolTraceSource::Standard), "Std"),
];

pub(super) struct Traces {
    source: Option<ToolTraceSource>,
    limit: usize,
    entries: huggler::Result<Vec<ToolTrace>>,
    scroll: UniformListScrollHandle,
}

impl Default for Traces {
    fn default() -> Self {
        Self {
            source: None,
            limit: PAGE_SIZE,
            entries: Ok(Vec::new()),
            scroll: UniformListScrollHandle::new(),
        }
    }
}

fn title(trace: &ToolTrace) -> &str {
    if !trace.item.title.trim().is_empty() {
        return &trace.item.title;
    }
    match trace.item.kind {
        ActivityKind::Command => "Shell command",
        ActivityKind::FileChange => "File change",
        ActivityKind::Search => "Web search",
        ActivityKind::Agent => "Agent tool",
        _ => "Tool call",
    }
}

/// Strip the provider's shell wrapper for display only. Keep the recorded command for Raw.
fn display_command(command: &str) -> std::borrow::Cow<'_, str> {
    let Some((shell, arguments)) = command.split_once(" -lc ") else {
        return command.into();
    };
    if !matches!(shell, "/bin/zsh" | "/bin/bash" | "/bin/sh") {
        return command.into();
    }
    match shlex::split(arguments) {
        Some(mut words) if words.len() == 1 => words.remove(0).into(),
        // Huggler 0.0.1 caps titles at 512 bytes, at a UTF-8 boundary.
        None if (509..=512).contains(&command.len()) => arguments
            .strip_prefix(['\'', '"'])
            .map_or_else(|| command.into(), |prefix| format!("{prefix}…").into()),
        _ => command.into(),
    }
}

fn display_target<'a>(trace: &'a ToolTrace, title: &'a str) -> std::borrow::Cow<'a, str> {
    if trace.source == ToolTraceSource::Mcp {
        return title.into();
    }
    match trace.item.kind {
        ActivityKind::Command => display_command(title),
        ActivityKind::FileChange => title.rsplit('/').next().unwrap_or(title).into(),
        _ => image_path_from_title(trace)
            .map(|path| path.rsplit('/').next().unwrap_or(path))
            .unwrap_or(title)
            .into(),
    }
}

fn summary(trace: &ToolTrace, target: &str) -> Option<(&'static str, IconName)> {
    if trace.source == ToolTraceSource::Mcp {
        return None;
    }
    Some(match trace.item.kind {
        ActivityKind::Command => {
            let mut words = target.split_whitespace();
            match (words.next(), words.next()) {
                (Some("rg"), Some("--files")) => ("List files", IconName::Folder),
                (Some("rg" | "grep"), _) => ("Search files", IconName::Search),
                (Some("sed"), Some("-i")) => ("Edit text", IconName::File),
                (Some("sed" | "cat" | "head" | "tail"), _) => ("Run text command", IconName::File),
                (Some("ls" | "pwd"), _) => ("Inspect files", IconName::Folder),
                (Some("git"), Some("status")) => ("Check Git status", IconName::SquareTerminal),
                (Some("git"), Some("diff")) => ("Inspect Git diff", IconName::SquareTerminal),
                (Some("git"), _) => ("Run Git", IconName::SquareTerminal),
                (Some("cargo"), Some("test")) => ("Run Rust tests", IconName::SquareTerminal),
                (Some("cargo"), Some("check")) => ("Check Rust code", IconName::SquareTerminal),
                (Some("cargo"), _) => ("Run Cargo", IconName::SquareTerminal),
                (Some("python" | "python3"), _) => ("Run Python", IconName::SquareTerminal),
                _ => ("Shell command", IconName::SquareTerminal),
            }
        }
        ActivityKind::FileChange => ("File changes", IconName::File),
        ActivityKind::Search => ("Web activity", IconName::Globe),
        ActivityKind::Tool if image_path_from_title(trace).is_some() => {
            ("View image", IconName::Eye)
        }
        _ => return None,
    })
}

// Huggler 0.0.1 records image views as generic tools with the path only in the title.
pub(super) fn image_path_from_title(trace: &ToolTrace) -> Option<&str> {
    (trace.source == ToolTraceSource::Standard && trace.item.kind == ActivityKind::Tool)
        .then(|| trace.item.title.strip_prefix("View image · "))
        .flatten()
        .filter(|path| !path.trim().is_empty())
}

pub(super) struct TracePresentation<'a> {
    pub title: &'a str,
    pub target: std::borrow::Cow<'a, str>,
    pub label: Option<&'static str>,
    icon: IconName,
}

impl<'a> TracePresentation<'a> {
    pub(super) fn new(trace: &'a ToolTrace) -> Self {
        let title = title(trace);
        let target = display_target(trace, title);
        let (label, icon) = match summary(trace, &target) {
            Some((label, icon)) => (Some(label), icon),
            None if trace.source == ToolTraceSource::Mcp => (None, IconName::Inspector),
            None if trace.item.kind == ActivityKind::Agent => (None, IconName::Bot),
            None => (None, IconName::Settings2),
        };
        Self {
            title,
            target,
            label,
            icon,
        }
    }

    pub(super) fn icon(&self, cx: &App) -> Icon {
        Icon::new(self.icon.clone())
            .xsmall()
            .flex_shrink_0()
            .text_color(cx.theme().muted_foreground)
    }
}

pub(super) fn source_label(source: ToolTraceSource) -> &'static str {
    match source {
        ToolTraceSource::Mcp => "MCP",
        ToolTraceSource::Standard => "Std",
        ToolTraceSource::Unknown => "Unknown source",
    }
}

pub(super) fn status(trace: &ToolTrace) -> &'static str {
    super::transcript::item_status(trace.item.status, trace.run_status).unwrap_or(
        if trace.item.status == ActivityStatus::Completed {
            "Completed"
        } else {
            "Unknown status"
        },
    )
}

pub(super) fn status_color(trace: &ToolTrace, cx: &App) -> Hsla {
    let theme = cx.theme();
    if trace.item.status == ActivityStatus::Failed {
        theme.danger.blend(theme.foreground.opacity(0.38))
    } else {
        theme.muted_foreground
    }
}

impl Desktop {
    pub(super) fn refresh_trace_list(&mut self) {
        if let Some(thread) = &self.current
            && let Some(workspace) = self.workspaces.get_mut(&thread.id)
        {
            let traces = &mut workspace.traces;
            traces.entries = self
                .runtime
                .tool_traces(&thread.id, traces.source, traces.limit + 1);
        }
    }

    pub(super) fn refresh_traces(&mut self, after: Option<i64>) {
        if let Some(thread) = &self.current
            && let Some(workspace) = self.workspaces.get_mut(&thread.id)
            && (self.workspace_tab == WorkspaceTab::Traces || workspace.documents.has_open_traces())
            && after.is_none_or(|cursor| {
                self.runtime
                    .tool_traces_changed_since(&thread.id, cursor)
                    .unwrap_or(true)
            })
        {
            workspace
                .documents
                .refresh_traces(&self.runtime, &thread.id);
            if self.workspace_tab == WorkspaceTab::Traces {
                self.refresh_trace_list();
            }
        }
    }

    pub(super) fn trace_panel(
        &self,
        thread: &ThreadId,
        panel_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let traces = &self.workspaces[thread].traces;
        let filters = h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .children(FILTERS.map(|(source, label)| {
                let selected = source == traces.source;
                control!(
                    format!("Trace filter/{label}"),
                    Button::new(SharedString::from(format!("trace-filter-{label}")))
                        .ghost()
                        .small()
                        .h_6()
                        .px_2()
                        .label(label)
                        .selected(selected)
                        .when(selected, |button| {
                            button
                                .bg(cx.theme().sidebar_accent)
                                .text_color(cx.theme().sidebar_accent_foreground)
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if let Some(thread) = &view.current {
                                let traces = &mut view
                                    .workspaces
                                    .get_mut(&thread.id)
                                    .expect("selected workspace")
                                    .traces;
                                if traces.source != source {
                                    traces.source = source;
                                    traces.limit = PAGE_SIZE;
                                    traces.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                                    view.refresh_trace_list();
                                    cx.notify();
                                }
                            }
                        }))
                )
            }));
        let entries = match &traces.entries {
            Err(error) => hint(format!("Could not load traces. {error}"), cx)
                .p_3()
                .into_any_element(),
            Ok(rows) if rows.is_empty() => {
                let message = match traces.source {
                    Some(ToolTraceSource::Mcp) => "No MCP tool calls yet",
                    Some(ToolTraceSource::Standard) => "No standard tool calls yet",
                    _ => "No recorded tool calls",
                };
                control!(message, hint(message, cx).p_3()).into_any_element()
            }
            Ok(rows) => {
                let row_thread = thread.clone();
                // The panel has a leading border and the scroll viewport has a focus border.
                let viewport_width = panel_width - px(1.) - LIST_MARGIN * 2.;
                let row_width = viewport_width - px(2.);
                let list = uniform_list(
                    "trace-list",
                    rows.len().min(traces.limit + 1),
                    cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                        let traces = &view.workspaces[&row_thread].traces;
                        let Ok(rows) = &traces.entries else {
                            return Vec::new();
                        };
                        range
                            .map(|index| {
                                if index == traces.limit {
                                    view.load_older_traces_row(cx)
                                } else {
                                    view.trace_row(&rows[index], row_width, window, cx)
                                }
                            })
                            .collect()
                    }),
                )
                .size_full()
                .with_sizing_behavior(ListSizingBehavior::Auto)
                .track_scroll(traces.scroll.clone());
                control!(
                    "Traces list",
                    keyboard_scroll_viewport(
                        SharedString::from(format!("traces-viewport-{thread}")),
                        &traces.scroll,
                        window,
                        cx,
                    )
                    .relative()
                    .size_full()
                    .child(list)
                    .vertical_scrollbar(&traces.scroll)
                )
                .flex_1()
                .min_h_0()
                .w(viewport_width)
                .m(LIST_MARGIN)
                .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(filters.flex_shrink_0())
            .child(entries)
    }

    fn load_older_traces_row(&self, cx: &Context<Self>) -> Div {
        v_flex().h(ROW_HEIGHT).justify_center().child(control!(
            "Load older traces",
            Button::new("load-older-traces")
                .ghost()
                .small()
                .label("Load older traces")
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some(thread) = &view.current {
                        view.workspaces
                            .get_mut(&thread.id)
                            .expect("selected workspace")
                            .traces
                            .limit += PAGE_SIZE;
                        view.refresh_trace_list();
                        cx.notify();
                    }
                }))
        ))
    }

    fn trace_row(
        &self,
        trace: &ToolTrace,
        width: Pixels,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Div {
        let presentation = TracePresentation::new(trace);
        let titled = single_line(presentation.title);
        let label = presentation
            .label
            .map_or_else(|| titled.clone(), str::to_owned);
        let target = single_line(&presentation.target);
        let preview = presentation.label.is_some()
            && !trace.item.title.trim().is_empty()
            && !target.trim().is_empty()
            && target != label;
        let failed = trace.item.status == ActivityStatus::Failed;
        let status = status(trace);
        let show_status = !failed && trace.item.status != ActivityStatus::Completed;
        let rem = window.rem_size();
        // Two half-rem insets, a half-rem gap, and the three-quarter-rem icon.
        let text_width = width - rem * 2.25;
        let status_width = if failed {
            FAILURE_DOT + rem * 0.375
        } else if show_status {
            let run = window.text_style().to_run(status.len());
            window
                .text_system()
                .layout_line(status, rem * 0.75, &[run], None)
                .width
                .ceil()
                + rem * 0.375
        } else {
            px(0.)
        };
        let tooltip = format!("{status} · {titled}");
        let key = SharedString::from(format!("trace-{}/{}", trace.run_id, trace.item.id));
        let document = Document::Trace(trace.clone());
        let selected = self
            .current
            .as_ref()
            .is_some_and(|thread| self.workspaces[&thread.id].documents.is_active(&document));

        let heading = h_flex()
            .gap_1p5()
            .line_height(px(18.))
            .child(TraceLabel {
                text: label.into(),
                width: text_width - status_width,
            })
            .when(failed, |row| {
                row.child(
                    div()
                        .size(FAILURE_DOT)
                        .flex_none()
                        .rounded_full()
                        .bg(status_color(trace, cx)),
                )
            })
            .when(show_status, |row| {
                row.child(hint(status, cx).text_xs().flex_none())
            });
        let body = v_flex()
            .min_w_0()
            .flex_1()
            .items_start()
            .overflow_hidden()
            .child(heading)
            .when(preview, |body| {
                body.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .text_xs()
                        .line_height(px(15.))
                        .when(trace.item.kind == ActivityKind::Command, |text| {
                            text.font_family(cx.theme().mono_font_family.clone())
                        })
                        .child(TraceLabel {
                            text: target.into(),
                            width: text_width,
                        }),
                )
            });

        control!(
            format!(
                "Trace/{}",
                presentation
                    .title
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Button::new(key)
                .ghost()
                .small()
                .h(ROW_HEIGHT)
                .w_full()
                .min_w_0()
                .justify_start()
                .px_0()
                .selected(selected)
                .when(selected, |button| button.bg(cx.theme().sidebar_accent))
                .tooltip(tooltip)
                .child(
                    h_flex()
                        // Button's content wrapper sizes to its children. Give it the list width
                        // so long titles and previews can truncate inside the row.
                        .w(width)
                        .px_2()
                        .gap_2()
                        .child(presentation.icon(cx))
                        .child(body),
                )
                .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                    view.open_sidebar_document(
                        document.clone(),
                        event.click_count() == 2,
                        window,
                        cx,
                    );
                })),
        )
        .h(ROW_HEIGHT)
        .w_full()
    }
}

#[derive(IntoElement)]
struct TraceLabel {
    text: SharedString,
    width: Pixels,
}

impl RenderOnce for TraceLabel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let run = style.to_run(self.text.len());
        // GPUI can measure and cache the untruncated text during intrinsic sizing.
        let text = cx
            .text_system()
            .line_wrapper(style.font(), font_size)
            .truncate_line(self.text, self.width, "…", &mut vec![run]);
        div().max_w(self.width).truncate().child(text)
    }
}

fn single_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}
