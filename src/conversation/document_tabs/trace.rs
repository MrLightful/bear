use super::*;

impl DocumentTabs {
    pub(in crate::conversation) fn has_open_traces(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| matches!(tab.document, Document::Trace(_)))
    }

    pub(in crate::conversation) fn refresh_traces(&mut self, runtime: &Huggler, thread: &ThreadId) {
        for tab in &mut self.tabs {
            let (Document::Trace(trace), Some(content)) = (&mut tab.document, &mut tab.trace)
            else {
                continue;
            };
            if !trace.run_status.terminal() {
                content.payload = read_trace(runtime, thread, trace);
            }
        }
    }
}

fn read_trace(
    runtime: &Huggler,
    thread: &ThreadId,
    trace: &mut ToolTrace,
) -> huggler::Result<ToolTracePayload> {
    let (updated, payload) = runtime.tool_trace(thread, &trace.run_id, &trace.item.id)?;
    *trace = updated;
    Ok(payload)
}

impl DocumentTab {
    pub(super) fn load_trace(
        &mut self,
        thread: &ThreadId,
        runtime: &Arc<Huggler>,
        cx: &mut Context<Desktop>,
    ) {
        let Document::Trace(trace) = &mut self.document else {
            return;
        };
        let payload = read_trace(runtime, thread, trace);
        if let Some(content) = &mut self.trace {
            content.payload = payload;
            return;
        }
        let reader = (trace.item.kind == huggler::ActivityKind::Command).then(|| {
            let (runtime, thread, run, item, id) = (
                runtime.clone(),
                thread.clone(),
                trace.run_id.clone(),
                trace.item.id.clone(),
                self.id,
            );
            cx.spawn(async move |view, cx| {
                let mut revision = None;
                loop {
                    let result = runtime.command_output(&thread, &run, &item, revision).await;
                    let done = result.as_ref().map_or(true, |output| output.closed);
                    if let Ok(output) = &result {
                        revision = Some(output.revision);
                    }
                    let found = view.update(cx, |view, cx| {
                        let content = view
                            .workspaces
                            .get_mut(&thread)
                            .and_then(|workspace| {
                                workspace.documents.tabs.iter_mut().find(|tab| tab.id == id)
                            })
                            .and_then(|tab| tab.trace.as_mut());
                        if let Some(content) = content {
                            content.output = Some(result);
                            cx.notify();
                            return true;
                        }
                        false
                    });
                    if done || !matches!(found, Ok(true)) {
                        return;
                    }
                }
            })
        });
        self.trace = Some(TraceContent {
            payload,
            output: None,
            _reader: reader,
        });
    }

    pub(super) fn render_trace(
        &self,
        trace: &ToolTrace,
        cx: &Context<Desktop>,
    ) -> impl IntoElement {
        let presentation = traces::TracePresentation::new(trace);
        let theme = cx.theme();
        let command = trace.item.kind == huggler::ActivityKind::Command;
        let mcp = trace.source == huggler::ToolTraceSource::Mcp;
        let standard_tool = trace.source == huggler::ToolTraceSource::Standard
            && trace.item.kind == huggler::ActivityKind::Tool;
        let mut details = v_flex().gap_5().px_4().py_4().w_full().max_w(px(760.));
        let content = self.trace.as_ref().expect("opened trace");
        let raw = self.mode == DocumentMode::Raw;
        let image_path = traces::image_path_from_title(trace);
        let mut has_summary = command && !trace.item.title.trim().is_empty();
        if has_summary {
            let recorded = &trace.item.title;
            let display = if raw {
                recorded.as_str()
            } else {
                presentation.target.as_ref()
            };
            let end = if raw {
                display.len()
            } else {
                let fourth_newline = display.match_indices('\n').nth(3).map(|(index, _)| index);
                let limit = display.floor_char_boundary(512);
                fourth_newline.map_or(limit, |index| index.min(limit))
            };
            details = details.child(
                trace_section("Command", &display[..end], None, false, cx)
                    .when(end < display.len(), |section| {
                        section.child(hint("See the recorded command in Raw", cx))
                    }),
            );
        }
        match &content.payload {
            Err(error) => {
                details = details.child(hint(format!("Could not read trace details. {error}"), cx));
            }
            Ok(payload) => {
                if let Some(input) = &payload.input {
                    let standard = (!mcp)
                        .then(|| standard_input(trace.item.kind, input))
                        .flatten();
                    let fields = (mcp || standard_tool)
                        .then(|| trace_fields(input))
                        .flatten();
                    has_summary |= standard.is_some() || fields.is_some();
                    if let Some((label, rows)) = standard.filter(|_| !raw) {
                        details = details.child(trace_section(label, input, Some(rows), false, cx));
                    } else {
                        details = details.child(trace_section(
                            "Input",
                            input,
                            fields.filter(|_| !raw),
                            false,
                            cx,
                        ));
                    }
                }
                if let Some(result) = &payload.result {
                    let fields = (mcp || standard_tool)
                        .then(|| trace_fields(result))
                        .flatten();
                    let acp_text = standard_tool.then(|| acp_text_result(result)).flatten();
                    has_summary |= fields.is_some() || acp_text.is_some();
                    let display = if raw {
                        result
                    } else {
                        acp_text.as_deref().unwrap_or(result)
                    };
                    details = details.child(trace_section(
                        "Result",
                        display,
                        fields.filter(|_| !raw),
                        trace.item.status == huggler::ActivityStatus::Failed,
                        cx,
                    ));
                }
                if let Some(path) = image_path {
                    details = details.child(trace_section("Image path", path, None, false, cx));
                } else if presentation.label.is_some()
                    && matches!(
                        trace.item.kind,
                        huggler::ActivityKind::FileChange | huggler::ActivityKind::Search
                    )
                    && payload.input.is_none()
                    && !trace.item.title.trim().is_empty()
                {
                    let label = if trace.item.kind == huggler::ActivityKind::FileChange {
                        "Changed files"
                    } else {
                        "Query"
                    };
                    details =
                        details.child(trace_section(label, presentation.title, None, false, cx));
                } else if !has_summary && payload.input.is_none() && payload.result.is_none() {
                    details = details.child(hint("No input or result recorded", cx));
                }
            }
        }
        if command {
            details = match &content.output {
                None => details.child(hint("Reading command output…", cx)),
                Some(Ok(output)) if !output.text.is_empty() => details.child(
                    trace_section("Output", &output.text, None, false, cx)
                        .when(output.truncated, |section| {
                            section.child(hint("Showing the latest 32 KiB", cx))
                        }),
                ),
                Some(Ok(output)) if !output.closed => {
                    details.child(hint("Waiting for command output…", cx))
                }
                Some(Ok(_)) | Some(Err(huggler::Error::NotFound(_))) => {
                    details.child(hint("No command output recorded", cx))
                }
                Some(Err(error)) => {
                    details.child(hint(format!("Could not read command output. {error}"), cx))
                }
            };
        }
        v_flex()
            .id(("trace-document", self.id))
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .min_w_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .size(px(32.))
                            .flex_none()
                            .justify_center()
                            .rounded_md()
                            .bg(theme.muted)
                            .child(presentation.icon(cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(control!(
                                format!("Tool trace/{}", presentation.title),
                                div()
                                    .text_base()
                                    .line_height(px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(
                                        presentation.label.unwrap_or(presentation.title).to_owned()
                                    )
                            ))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(hint(traces::source_label(trace.source), cx).text_xs())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(traces::status_color(trace, cx))
                                            .child(traces::status(trace)),
                                    ),
                            ),
                    )
                    .when(has_summary, |header| {
                        header.child(self.mode_control(
                            &[
                                (
                                    DocumentMode::Preview,
                                    if mcp { "Fields" } else { "Summary" },
                                ),
                                (DocumentMode::Raw, "Raw"),
                            ],
                            cx,
                        ))
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("trace-content")
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&self.scroll)
                            .child(details),
                    )
                    .scrollbar(&self.scroll, ScrollbarAxis::Both),
            )
    }
}

type TraceFields = Vec<(String, serde_json::Value)>;

fn trace_fields(text: &str) -> Option<TraceFields> {
    let fields = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text).ok()?;
    (!fields.is_empty()).then(|| fields.into_iter().collect())
}

fn standard_input(kind: huggler::ActivityKind, input: &str) -> Option<(&'static str, TraceFields)> {
    use huggler::ActivityKind;
    use serde_json::Value;

    match kind {
        ActivityKind::FileChange => {
            let changes: Vec<Value> = serde_json::from_str(input).ok()?;
            let rows = changes
                .into_iter()
                .map(|change| {
                    let path = change["path"].as_str()?;
                    let label = match change["kind"]["type"].as_str() {
                        Some("add") => "Added",
                        Some("delete") => "Deleted",
                        Some("update") if change["kind"]["move_path"].is_string() => "Moved",
                        Some("update") => "Modified",
                        _ => "Path",
                    };
                    let target = match change["kind"]["move_path"].as_str() {
                        Some(target) => format!("{path} → {target}"),
                        None => path.to_owned(),
                    };
                    Some((label.to_owned(), Value::String(target)))
                })
                .collect::<Option<Vec<_>>>()?;
            (!rows.is_empty()).then_some(("Changed files", rows))
        }
        ActivityKind::Search => {
            let action: Value = serde_json::from_str(input).ok()?;
            let (label, keys) = match action["type"].as_str()? {
                "search" => (
                    "Search the web",
                    &[("query", "Query"), ("queries", "Query")][..],
                ),
                "openPage" => ("Open web page", &[("url", "URL")][..]),
                "findInPage" => (
                    "Find in page",
                    &[("url", "URL"), ("pattern", "Pattern")][..],
                ),
                _ => return None,
            };
            let rows: Vec<_> = keys
                .iter()
                .flat_map(|&(key, label)| {
                    match &action[key] {
                        Value::Array(values) => {
                            values.iter().filter(|value| value.is_string()).collect()
                        }
                        value @ Value::String(_) => vec![value],
                        _ => Vec::new(),
                    }
                    .into_iter()
                    .map(move |value| (label.to_owned(), value.clone()))
                })
                .collect();
            (!rows.is_empty()).then_some((label, rows))
        }
        _ => None,
    }
}

// ACP results may contain nested content blocks. Keep mixed or unknown blocks in Raw.
fn acp_text_result(result: &str) -> Option<String> {
    let entries = serde_json::from_str::<Vec<serde_json::Value>>(result).ok()?;
    if entries.is_empty() {
        return None;
    }
    let blocks = entries
        .iter()
        .map(|entry| {
            let content = &entry["content"];
            if entry["type"] == "content" && content["type"] == "text" {
                content["text"].as_str()
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()?;
    Some(blocks.join("\n\n"))
}

fn trace_section(
    label: &'static str,
    text: &str,
    fields: Option<TraceFields>,
    failed: bool,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let body = match fields {
        Some(fields) => {
            v_flex().children(fields.into_iter().enumerate().map(|(index, (key, value))| {
                let value = match value {
                    serde_json::Value::String(value) if !value.is_empty() => value,
                    serde_json::Value::Array(blocks) if label == "Result" && key == "content" => {
                        text_content(&blocks).unwrap_or_else(|| {
                            serde_json::to_string_pretty(&blocks).unwrap_or_default()
                        })
                    }
                    _ => serde_json::to_string_pretty(&value).unwrap_or_default(),
                };
                h_flex()
                    .items_start()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .when(index > 0, |field| {
                        field.border_t_1().border_color(theme.border)
                    })
                    .child(
                        control!(
                            format!("Trace field/{key}"),
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(key)
                        )
                        .w(relative(0.3))
                        .max_w(px(112.))
                        .flex_none(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .line_height(px(20.))
                            .child(StyledText::new(value)),
                    )
            }))
        }
        None => div()
            .px_3()
            .py_2()
            .whitespace_nowrap()
            .font_family(theme.mono_font_family.clone())
            .text_sm()
            .line_height(px(20.))
            .child(StyledText::new(text.to_owned())),
    };
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_2()
                .when(failed, |heading| {
                    heading.child(
                        Icon::new(IconName::TriangleAlert)
                            .xsmall()
                            .text_color(theme.danger),
                    )
                })
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(label),
                ),
        )
        .child(control!(
            format!("Trace {label}/{}", &text[..text.floor_char_boundary(1024)]),
            div()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.sidebar)
                .child(body)
        ))
}

fn text_content(blocks: &[serde_json::Value]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }
    let texts = blocks
        .iter()
        .map(|block| {
            (block["type"] == "text")
                .then(|| block["text"].as_str())
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    Some(texts.join("\n\n"))
}
