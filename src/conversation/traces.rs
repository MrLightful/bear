use super::*;
use crate::ui::keyboard_scroll_viewport;
use gpui_component::scroll::ScrollableElement;
use huggler::{ActivityKind, ActivityStatus, ToolTrace, ToolTraceSource};

const PAGE_SIZE: usize = 50;
const ROW_HEIGHT: Pixels = px(42.);
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

pub(super) fn title(trace: &ToolTrace) -> &str {
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

pub(super) fn icon(trace: &ToolTrace, cx: &App) -> Icon {
    let name = match trace.item.kind {
        _ if trace.source == ToolTraceSource::Mcp => IconName::Inspector,
        ActivityKind::Command => IconName::SquareTerminal,
        ActivityKind::FileChange => IconName::File,
        ActivityKind::Search => IconName::Search,
        ActivityKind::Agent => IconName::Bot,
        _ => IconName::Settings2,
    };
    Icon::new(name)
        .xsmall()
        .flex_shrink_0()
        .text_color(cx.theme().muted_foreground)
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
                let list = uniform_list(
                    "trace-list",
                    rows.len().min(traces.limit + 1),
                    cx.processor(move |view, range: std::ops::Range<usize>, _, cx| {
                        let traces = &view.workspaces[&row_thread].traces;
                        let Ok(rows) = &traces.entries else {
                            return Vec::new();
                        };
                        range
                            .map(|index| {
                                if index == traces.limit {
                                    view.load_older_traces_row(cx)
                                } else {
                                    view.trace_row(&rows[index], cx)
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
                .m_1()
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

    fn trace_row(&self, trace: &ToolTrace, cx: &Context<Self>) -> Div {
        let titled = title(trace)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let key = SharedString::from(format!("trace-{}/{}", trace.run_id, trace.item.id));
        let document = Document::Trace(trace.clone());
        let selected = self
            .current
            .as_ref()
            .is_some_and(|thread| self.workspaces[&thread.id].documents.is_active(&document));

        control!(
            format!("Trace/{titled}"),
            Button::new(key)
                .ghost()
                .small()
                .h(ROW_HEIGHT)
                .w_full()
                .min_w_0()
                .justify_start()
                .px_2()
                .gap_3()
                .selected(selected)
                .when(selected, |button| button.bg(cx.theme().sidebar_accent))
                .tooltip(titled.clone())
                .child(icon(trace, cx))
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .items_start()
                        .child(
                            div()
                                .max_w_full()
                                .line_height(px(18.))
                                .truncate()
                                .child(titled)
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .max_w_full()
                                .overflow_hidden()
                                .text_xs()
                                .line_height(px(14.))
                                .child(hint(source_label(trace.source), cx).text_xs().truncate())
                                .child(
                                    div()
                                        .text_color(status_color(trace, cx))
                                        .truncate()
                                        .child(status(trace))
                                )
                        ),
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
