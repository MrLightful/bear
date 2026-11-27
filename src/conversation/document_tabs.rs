mod diff;
mod editor;
mod media;
mod trace;

use super::{markdown_view::Markdown, traces, *};
use crate::{
    changes::{ReviewRow, line_counts},
    file_icons,
    git::PreparedRevert,
};
use anyhow::{Result, ensure};
use diff::Diff;
pub(super) use editor::open_workspace_file;
use editor::{Editor, confirm_discard};
use gpui_component::{
    IconNamed, WindowExt,
    button::ButtonCustomVariant,
    scroll::{ScrollableElement, ScrollbarAxis},
    tooltip::Tooltip,
};
use huggler::{ToolTrace, ToolTracePayload};
use std::{
    cell::{Cell, RefCell},
    io::Read,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

pub(crate) const DOCUMENT_CONTEXT: &str = "HugglerDocument";
const TEXT_LIMIT: usize = 128 * 1024;
const TAB_LABEL_WIDTH: Pixels = px(144.);
const COPY_FEEDBACK: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(super) enum Document {
    File(Arc<Path>),
    Diff(ReviewRow),
    Trace(ToolTrace),
}

impl Document {
    fn path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Diff(row) => Some(&row.file.path),
            Self::Trace(_) => None,
        }
    }

    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::File(a), Self::File(b)) => a == b,
            (Self::Diff(a), Self::Diff(b)) => a.file.path == b.file.path && a.scope == b.scope,
            (Self::Trace(a), Self::Trace(b)) => a.run_id == b.run_id && a.item.id == b.item.id,
            _ => false,
        }
    }

    fn comparison(&self) -> Option<&'static str> {
        match self {
            Self::File(_) | Self::Trace(_) => None,
            Self::Diff(row) => Some(match row.scope {
                git::ChangeScope::All => "All changes",
                git::ChangeScope::Uncommitted => "Uncommitted files",
            }),
        }
    }

    fn initial_mode(&self) -> DocumentMode {
        match self {
            Self::Diff(_) => DocumentMode::Unified,
            Self::File(_) | Self::Trace(_) => DocumentMode::Preview,
        }
    }

    fn read_diff(root: &Path, row: &ReviewRow, refresh: bool) -> Result<LoadedDiff> {
        let row = if refresh {
            let snapshot = git::changed_files(root, row.scope)?;
            snapshot
                .files
                .into_iter()
                .find(|file| file.path == row.file.path)
                .map(|file| ReviewRow {
                    file,
                    base: snapshot.base,
                    head: snapshot.head,
                    scope: row.scope,
                })
        } else {
            Some(row.clone())
        };
        let Some(row) = row else {
            return Ok(LoadedDiff {
                patch: Preview::Notice(NO_DIFF),
                review: None,
                revert: None,
            });
        };
        let revert = row
            .file
            .can_revert()
            .then(|| PreparedRevert::prepare(root, row.head.clone(), row.file.clone()).ok())
            .flatten()
            .map(|target| Arc::from([target]));
        let bytes = git::file_patch(root, &row.base, &row.head, &row.file, TEXT_LIMIT + 1)?;
        let patch = if bytes.is_empty() {
            Preview::Notice(NO_DIFF)
        } else {
            Preview::text(bytes, Some(&row.file.path), false)
        };
        Ok(LoadedDiff {
            patch,
            review: Some(row),
            revert,
        })
    }
}

#[derive(Default)]
pub(super) struct DocumentTabs {
    tabs: Vec<DocumentTab>,
    editors: HashMap<Arc<Path>, Editor>,
    active: Option<usize>,
    next_id: usize,
    scroll: ScrollHandle,
    reveal: Rc<Cell<Option<usize>>>,
}

impl DocumentTabs {
    pub(super) fn show_chat(&mut self) {
        self.active = None;
    }

    pub(super) fn active(&self) -> Option<&DocumentTab> {
        self.active.and_then(|index| self.tabs.get(index))
    }

    fn active_mut(&mut self, id: usize) -> Option<&mut DocumentTab> {
        self.tabs.get_mut(self.active?).filter(|tab| tab.id == id)
    }

    pub(super) fn is_active(&self, document: &Document) -> bool {
        self.active()
            .is_some_and(|tab| tab.document.matches(document))
    }

    pub(super) fn active_editor_focus(&self, cx: &App) -> Option<FocusHandle> {
        let tab = self.active().filter(|tab| tab.mode == DocumentMode::Raw)?;
        let draft = self.editors.get(tab.document.path()?)?.draft()?;
        (!draft.is_saving()).then(|| draft.input.focus_handle(cx))
    }

    fn index_of(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    fn remove_unused_editor(&mut self, tab: &DocumentTab) {
        if let Some(path) = tab.document.path()
            && !self
                .tabs
                .iter()
                .any(|other| other.document.path() == Some(path))
        {
            self.editors.remove(path);
        }
    }

    fn select(&mut self, index: usize) {
        self.active = Some(index);
        self.reveal.set(Some(index));
    }
}

const NO_DIFF: &str = "This file has no changes in this comparison.";

pub(super) struct DocumentTab {
    id: usize,
    document: Document,
    preview: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    patch_scroll: ScrollHandle,
    pending_heading: RefCell<Option<String>>,
    diff_scroll: UniformListScrollHandle,
    mode: DocumentMode,
    copied_at: Option<Instant>,
    file_preview: Option<Result<Preview>>,
    diff: Option<Result<LoadedDiff>>,
    revert_error: Option<String>,
    file_load: Option<Task<()>>,
    diff_load: Option<Task<()>>,
    trace: Option<TraceContent>,
}

struct TraceContent {
    payload: huggler::Result<ToolTracePayload>,
    output: Option<huggler::Result<huggler::CommandOutput>>,
    _reader: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentMode {
    Preview,
    Unified,
    Patch,
    Raw,
}

struct LoadedDiff {
    patch: Preview,
    review: Option<ReviewRow>,
    revert: Option<Arc<[PreparedRevert]>>,
}

enum Formatted {
    Markdown(Box<Markdown>),
    Diff(Arc<Diff>),
}

enum Preview {
    Text {
        text: SharedString,
        truncated: bool,
        formatted: Option<Formatted>,
    },
    Image {
        thumbnail: Arc<RenderImage>,
        source: Arc<Image>,
    },
    #[cfg(target_os = "macos")]
    Pdf {
        pages: Vec<Option<Arc<RenderImage>>>,
        total: usize,
    },
    Notice(&'static str),
}

impl Preview {
    fn text(bytes: Vec<u8>, diff: Option<&Path>, markdown: bool) -> Self {
        let truncated = bytes.len() > TEXT_LIMIT;
        let bytes = &bytes[..bytes.len().min(TEXT_LIMIT)];
        let text = match std::str::from_utf8(bytes) {
            Ok(text) => std::borrow::Cow::Borrowed(text),
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .expect("valid UTF-8 prefix")
                    .into()
            }
            Err(_) if diff.is_some() => String::from_utf8_lossy(bytes),
            Err(_) => return Self::Notice("A text preview is unavailable for this file."),
        };
        if diff.is_none() && text.contains('\0') {
            return Self::Notice("A text preview is unavailable for this file.");
        }
        let formatted = if let Some(path) = diff {
            Some(Formatted::Diff(Arc::new(Diff::new(&text, path))))
        } else if markdown {
            Some(Formatted::Markdown(Box::new(Markdown::new(&text))))
        } else {
            None
        };
        Self::Text {
            text: text.into_owned().into(),
            truncated,
            formatted,
        }
    }

    fn copy_content(&self, diff: bool) -> Option<(&'static str, CopyContent)> {
        match self {
            Self::Text {
                text, truncated, ..
            } => {
                let tooltip = if *truncated {
                    "Copy excerpt"
                } else if diff {
                    "Copy diff"
                } else {
                    "Copy file"
                };
                Some((tooltip, CopyContent::Text(text.clone())))
            }
            Self::Image { source, .. } => Some(("Copy image", CopyContent::Image(source.clone()))),
            #[cfg(target_os = "macos")]
            Self::Pdf { .. } => None,
            Self::Notice(_) => None,
        }
    }
}

enum CopyContent {
    Text(SharedString),
    Image(Arc<Image>),
    Draft(Entity<InputState>),
}

impl CopyContent {
    fn clipboard_item(&self, cx: &App) -> ClipboardItem {
        match self {
            Self::Text(text) => ClipboardItem::new_string(text.to_string()),
            Self::Image(image) => ClipboardItem::new_image(image),
            Self::Draft(input) => ClipboardItem::new_string(input.read(cx).value().to_string()),
        }
    }
}

fn read_file(root: &Path, path: &Path) -> Result<Preview> {
    let target = root.canonicalize()?.join(path);
    let parent = target.parent().expect("workspace file has a parent");
    ensure!(
        parent.canonicalize()? == parent,
        "The parent folder has moved."
    );
    let kind = target.symlink_metadata()?.file_type();
    if kind.is_symlink() {
        return Ok(Preview::Text {
            text: format!("Symbolic link → {}", fs::read_link(target)?.display()).into(),
            truncated: false,
            formatted: None,
        });
    }
    ensure!(
        kind.is_file(),
        "The selected path no longer points to a regular file."
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(target)?;
    ensure!(
        file.metadata()?.is_file(),
        "The selected path no longer points to a regular file."
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take((TEXT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Ok(Preview::Notice("This file is empty."));
    }
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if let Some(media) = media::Media::detect(&bytes, extension) {
        return Ok(media.read(file, bytes)?);
    }
    let markdown = ["md", "markdown", "mdx"]
        .iter()
        .any(|name| extension.eq_ignore_ascii_case(name));
    Ok(Preview::text(bytes, None, markdown))
}

impl Desktop {
    fn active_document_mut(&mut self, id: usize) -> Option<&mut DocumentTab> {
        let thread = self.current.as_ref()?;
        let tabs = &mut self.workspaces.get_mut(&thread.id)?.documents;
        tabs.active_mut(id)
    }

    pub(super) fn return_to_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(thread) = &self.current {
            self.workspaces
                .get_mut(&thread.id)
                .expect("selected workspace")
                .documents
                .show_chat();
        }
        self.focus_workspace(window, cx);
        cx.notify();
    }

    pub(super) fn open_sidebar_document(
        &mut self,
        document: Document,
        keep_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_document(document, None, !keep_open, window, cx);
    }

    pub(super) fn open_document_at_heading(
        &mut self,
        document: Document,
        heading: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_document(document, heading, false, window, cx);
    }

    fn open_document(
        &mut self,
        document: Document,
        heading: Option<String>,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.current.as_ref().map(|thread| thread.id.clone()) else {
            return;
        };
        self.suggestions = Default::default();
        self.model_picker = None;
        if window.viewport_size().width < NARROW_VIEWPORT_WIDTH && self.right_sidebar.is_open(false)
        {
            self.right_sidebar.toggle(true);
        }
        let workspace = self
            .workspaces
            .get_mut(&thread)
            .expect("selected workspace");
        let tabs = &mut workspace.documents;
        let index = tabs
            .tabs
            .iter()
            .position(|tab| tab.document.matches(&document))
            .unwrap_or_else(|| {
                let tab = DocumentTab {
                    id: tabs.next_id,
                    document: document.clone(),
                    preview,
                    focus: cx.focus_handle().tab_stop(true),
                    scroll: ScrollHandle::new(),
                    patch_scroll: ScrollHandle::new(),
                    diff_scroll: UniformListScrollHandle::new(),
                    pending_heading: RefCell::default(),
                    mode: document.initial_mode(),
                    copied_at: None,
                    file_preview: None,
                    diff: None,
                    revert_error: None,
                    file_load: None,
                    diff_load: None,
                    trace: None,
                };
                tabs.next_id += 1;
                if preview && let Some(index) = tabs.tabs.iter().position(|tab| tab.preview) {
                    let replaced = std::mem::replace(&mut tabs.tabs[index], tab);
                    tabs.remove_unused_editor(&replaced);
                    index
                } else {
                    let index = tabs.tabs.len();
                    tabs.tabs.push(tab);
                    index
                }
            });
        tabs.select(index);
        let tab = &mut tabs.tabs[index];
        tab.document = document;
        tab.preview &= preview;
        if heading.is_some() {
            tab.mode = DocumentMode::Preview;
        }
        *tab.pending_heading.get_mut() = heading;
        tab.revert_error = None;
        let editor_path = if let Some(path) = tab.document.path() {
            let editor_path = (tab.mode == DocumentMode::Raw || tabs.editors.contains_key(path))
                .then(|| Arc::from(path));
            tab.load(&thread, &workspace.root, false, cx);
            editor_path
        } else {
            tab.load_trace(&thread, &self.runtime, cx);
            None
        };
        self.focus_workspace(window, cx);
        if let Some(path) = editor_path {
            self.load_raw_editor(&thread, path, true, window, cx);
        }
        cx.notify();
    }

    fn keep_document_open(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(thread) = &self.current {
            let tabs = &mut self
                .workspaces
                .get_mut(&thread.id)
                .expect("selected workspace")
                .documents;
            if let Some(index) = tabs.index_of(id) {
                let tab = &mut tabs.tabs[index];
                tab.preview = false;
                tab.focus.focus(window);
                cx.notify();
            }
        }
    }

    fn open_active_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.current.as_ref().map(|current| current.id.clone()) else {
            return;
        };
        if let Some(tab) = self.workspaces[&thread].documents.active()
            && tab.mode == DocumentMode::Raw
            && let Some(path) = tab.document.path()
        {
            self.load_raw_editor(&thread, path.into(), false, window, cx);
        }
    }

    pub(super) fn refresh_reverted_documents(
        &mut self,
        thread: &ThreadId,
        result: &RevertFinished,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspaces.get_mut(thread) else {
            return;
        };
        if !result.paths.is_empty() {
            workspace.files.update(cx, FileTree::refresh);
        }
        for tab in &mut workspace.documents.tabs {
            let Some(path) = tab.document.path() else {
                continue;
            };
            let error = result.failure.as_ref().filter(|(failed, _)| failed == path);
            if error.is_none() && !result.paths.iter().any(|changed| changed == path) {
                continue;
            }
            tab.revert_error = error.map(|(_, message)| message.clone());
            tab.load(thread, &workspace.root, true, cx);
        }
        let refresh: Vec<_> = result
            .paths
            .iter()
            .filter(|path| workspace.documents.editors.contains_key(path.as_path()))
            .map(|path| Arc::<Path>::from(path.as_path()))
            .collect();
        for path in refresh {
            self.load_raw_editor(thread, path, true, window, cx);
        }
        cx.notify();
    }

    fn close_document(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = &self.current else {
            return;
        };
        let tabs = &mut self
            .workspaces
            .get_mut(&thread.id)
            .expect("selected workspace")
            .documents;
        let Some(index) = tabs.index_of(id) else {
            return;
        };
        let Some(path) = tabs.tabs[index].document.path() else {
            return self.remove_document(id, window, cx);
        };
        let last = !tabs
            .tabs
            .iter()
            .enumerate()
            .any(|(other, tab)| other != index && tab.document.path() == Some(path));
        if last && let Some(draft) = tabs.editors.get(path).and_then(Editor::draft) {
            if draft.is_saving() || window.has_active_dialog(cx) {
                return;
            }
            if draft.dirty {
                let thread = thread.id.clone();
                let view = cx.weak_entity();
                confirm_discard(
                    "Discard unsaved changes?",
                    format!("Changes to {} will be discarded.", path.display()),
                    "Discard changes",
                    move |window, cx| {
                        let _ = view.update(cx, |view, cx| {
                            if view
                                .current
                                .as_ref()
                                .is_some_and(|current| current.id == thread)
                            {
                                view.remove_document(id, window, cx);
                            }
                        });
                    },
                    window,
                    cx,
                );
                return;
            }
        }
        self.remove_document(id, window, cx);
    }

    fn remove_document(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = &self.current else {
            return;
        };
        let tabs = &mut self
            .workspaces
            .get_mut(&thread.id)
            .expect("selected workspace")
            .documents;
        let Some(index) = tabs.index_of(id) else {
            return;
        };
        let tab = tabs.tabs.remove(index);
        tabs.remove_unused_editor(&tab);
        match tabs.active {
            Some(active) if active == index && index > 0 => tabs.select(index - 1),
            Some(active) if active == index => return self.return_to_chat(window, cx),
            Some(active) => tabs.select(active - usize::from(active > index)),
            None => {}
        }
        self.focus_workspace(window, cx);
        self.open_active_editor(window, cx);
        cx.notify();
    }

    pub(super) fn close_active_document(
        &mut self,
        _: &crate::CloseDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self
            .current
            .as_ref()
            .and_then(|thread| self.workspaces[&thread.id].documents.active())
        {
            self.close_document(tab.id, window, cx);
        }
    }

    pub(super) fn document_tab_strip(&self, window: &Window, cx: &Context<Self>) -> Div {
        let documents = self
            .current
            .as_ref()
            .map(|thread| &self.workspaces[&thread.id].documents);
        let chat_active = documents.and_then(DocumentTabs::active).is_none();
        let theme = cx.theme();
        h_flex()
            .relative()
            .h_9()
            .items_end()
            .flex_shrink_0()
            .px_2()
            .gap_0p5()
            .bg(theme.sidebar)
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h_px()
                    .bg(theme.border),
            )
            .child(tab_surface(chat_active, cx).child(control!(
                "Chat tab",
                Button::new("chat-tab")
                    .custom(ButtonCustomVariant::new(cx).foreground(if chat_active {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    }))
                    .small()
                    .h_full()
                    .px_3()
                    .icon(Icon::default().path("icons/chat.svg"))
                    .label("Chat")
                    .on_click(cx.listener(|view, _, window, cx| view.return_to_chat(window, cx)))
            )))
            .when_some(
                documents.filter(|documents| !documents.tabs.is_empty()),
                |strip, documents| {
                    strip.child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .on_children_prepainted({
                                let reveal = documents.reveal.clone();
                                let scroll = documents.scroll.clone();
                                move |_, window, _| reveal_tab(&reveal, &scroll, window)
                            })
                            .child(
                                h_flex()
                                    .id("document-tabs")
                                    .h_8()
                                    .gap_0p5()
                                    .overflow_x_scroll()
                                    .track_scroll(&documents.scroll)
                                    .children(documents.tabs.iter().enumerate().map(
                                        |(index, tab)| {
                                            tab.selector(
                                                documents.active == Some(index),
                                                tab.document
                                                    .path()
                                                    .and_then(|path| documents.editors.get(path))
                                                    .and_then(Editor::draft)
                                                    .is_some_and(|draft| draft.dirty),
                                                window,
                                                cx,
                                            )
                                        },
                                    )),
                            )
                            .horizontal_scrollbar(&documents.scroll),
                    )
                },
            )
    }
}

pub(super) fn tab_icon_button(
    id: &'static str,
    icon_path: impl Into<SharedString>,
    icon_size: impl Into<Length>,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    let foreground = theme.foreground;
    let icon_size = icon_size.into();
    Button::new(id)
        .custom(ButtonCustomVariant::new(cx))
        .group(id)
        // The tab beneath the action should keep its current background.
        .block_mouse_except_scroll()
        .child(
            svg()
                .path(icon_path)
                .size(icon_size)
                .flex_none()
                .text_color(theme.muted_foreground)
                .group_hover(id, move |style| style.text_color(foreground)),
        )
}

pub(super) fn tab_surface(active: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let hover = theme.muted;
    h_flex()
        .h_8()
        .flex_none()
        .rounded_t_md()
        .border_t_1()
        .border_l_1()
        .border_r_1()
        .border_color(transparent_black())
        .when(active, |tab| {
            tab.bg(theme.background).border_color(theme.border)
        })
        .when(!active, |tab| tab.hover(move |style| style.bg(hover)))
}

impl DocumentTab {
    fn selector(
        &self,
        active: bool,
        dirty: bool,
        window: &Window,
        cx: &Context<Desktop>,
    ) -> impl IntoElement {
        let id = self.id;
        let diff = self.document.comparison();
        let (name, full_title, _key, icon) = match &self.document {
            Document::Trace(trace) => {
                let presentation = traces::TracePresentation::new(trace);
                let icon = presentation.icon(cx).into_any_element();
                (
                    presentation.target.into_owned(),
                    presentation.title.to_owned(),
                    format!("Trace tab/{}", presentation.title),
                    icon,
                )
            }
            document => {
                let path = document.path().expect("working file path");
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let full_path = path.display().to_string();
                let key = diff.map_or_else(
                    || format!("File tab/{full_path}"),
                    |scope| format!("Diff tab/{scope}/{full_path}"),
                );
                (
                    name,
                    full_path,
                    key,
                    file_icons::icon(path, cx.theme().muted_foreground),
                )
            }
        };
        let name = name.replace(['\n', '\r', '\t'], " ");
        let theme = cx.theme();
        let ring = theme.ring;
        let muted = theme.muted_foreground;
        let label_color = if active { theme.foreground } else { muted };
        let label_style = TextStyle {
            font_family: theme.font_family.clone(),
            color: label_color,
            font_style: if self.preview {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..Default::default()
        };
        let font_size = crate::ui::control_text_size().to_pixels(window.rem_size());
        let mut runs = vec![label_style.to_run(name.len())];
        let label_width = window
            .text_system()
            .layout_line(&name, font_size, &runs, None)
            .width
            .ceil();
        let truncated = label_width > TAB_LABEL_WIDTH;
        let tooltip = match diff {
            Some(scope) => Some(format!("{full_title} · {scope}")),
            None if full_title != name || truncated => Some(full_title),
            None => None,
        };
        let name = if truncated {
            cx.text_system()
                .line_wrapper(label_style.font(), font_size)
                .truncate_line(name.into(), TAB_LABEL_WIDTH, "…", &mut runs)
        } else {
            name.into()
        };
        tab_surface(active, cx)
            .id(("document-tab", id))
            .pr_0p5()
            .child(
                control!(
                    _key.clone(),
                    h_flex()
                        .id("select")
                        .track_focus(&self.focus)
                        .h_7()
                        .gap_1p5()
                        .pl_2()
                        .pr_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(transparent_black())
                        .focus(move |style| style.border_color(ring))
                        .cursor_pointer()
                        .when_some(tooltip, |tab, tooltip| tab.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        }))
                        .child(icon)
                        .child(
                            div()
                                .w(label_width.min(TAB_LABEL_WIDTH))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_size(crate::ui::control_text_size())
                                .text_color(label_color)
                                .when(self.preview, |label| label.italic())
                                .child(name)
                        )
                        .when(diff.is_some(), |tab| tab.child(
                            div().flex_none().text_xs().text_color(muted).child("Diff")
                        ))
                        .when(dirty, |tab| {
                            tab.child(div().size(px(5.)).flex_none().rounded_full().bg(muted))
                        })
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                            if let Some(thread) = &view.current {
                                let tabs = &mut view
                                    .workspaces
                                    .get_mut(&thread.id)
                                    .expect("selected workspace")
                                    .documents;
                                if let Some(index) = tabs.index_of(id) {
                                    tabs.select(index);
                                    if event.click_count() == 2 {
                                        tabs.tabs[index].preview = false;
                                    }
                                    if !event.is_keyboard() {
                                        view.focus_workspace(window, cx);
                                    }
                                    view.open_active_editor(window, cx);
                                    cx.notify();
                                }
                            }
                        }))
                )
                .flex_none(),
            )
            .when(self.preview, |tab| {
                tab.child(
                    control!(
                        format!("Keep open {_key}"),
                        tab_icon_button("keep-open", "icons/pin.svg", px(12.), cx)
                            .size(px(24.))
                            .p_0()
                            .tooltip("Keep Open")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.keep_document_open(id, window, cx)
                            }))
                    )
                    .flex_none(),
                )
            })
            .child(
                control!(
                    format!("Close {_key}"),
                    tab_icon_button("close", IconName::Close.path(), px(12.), cx)
                        .size(px(24.))
                        .p_0()
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.close_document(id, window, cx)
                        }))
                )
                .flex_none(),
            )
    }

    fn load(&mut self, thread: &ThreadId, root: &Path, refresh: bool, cx: &mut Context<Desktop>) {
        self.copied_at = None;
        self.file_preview = None;
        self.file_load = None;
        if self.mode == DocumentMode::Preview {
            self.load_file_preview(thread, root, cx);
        }
        let Document::Diff(row) = &self.document else {
            return;
        };
        let row = row.clone();
        let thread = thread.clone();
        let root = root.to_path_buf();
        let id = self.id;
        let job = cx.background_spawn(async move { Document::read_diff(&root, &row, refresh) });
        self.diff_load = Some(cx.spawn(async move |view, cx| {
            let result = job.await;
            let _ = view.update(cx, |view, cx| {
                if let Some(tab) = view.workspaces.get_mut(&thread).and_then(|workspace| {
                    workspace.documents.tabs.iter_mut().find(|tab| tab.id == id)
                }) {
                    tab.diff = Some(result);
                    tab.diff_load = None;
                    cx.notify();
                }
            });
        }));
    }

    fn load_file_preview(&mut self, thread: &ThreadId, root: &Path, cx: &mut Context<Desktop>) {
        if self.file_preview.is_some() || self.file_load.is_some() {
            return;
        }
        let Some(path) = self.document.path().map(Path::to_path_buf) else {
            return;
        };
        let thread = thread.clone();
        let root = root.to_path_buf();
        let id = self.id;
        let job = cx.background_spawn(async move { read_file(&root, &path) });
        self.file_load = Some(cx.spawn(async move |view, cx| {
            let result = job.await;
            let _ = view.update(cx, |view, cx| {
                if let Some(tab) = view.workspaces.get_mut(&thread).and_then(|workspace| {
                    workspace.documents.tabs.iter_mut().find(|tab| tab.id == id)
                }) {
                    tab.file_preview = Some(result);
                    tab.file_load = None;
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn render(
        &self,
        changes: &Entity<Changes>,
        documents: &DocumentTabs,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> AnyElement {
        if let Document::Trace(trace) = &self.document {
            return self.render_trace(trace, cx).into_any_element();
        }
        let path = self.document.path().expect("working file path");
        let path_label = path.display().to_string();
        let scope = self.document.comparison();
        let theme = cx.theme();
        let (border, muted, foreground) = (theme.border, theme.muted_foreground, theme.foreground);
        let diff = self.diff.as_ref();
        let review = diff.and_then(|diff| diff.as_ref().ok());
        let viewing_diff = matches!(self.mode, DocumentMode::Unified | DocumentMode::Patch);
        let content_preview = if viewing_diff {
            diff.map(|diff| diff.as_ref().map(|diff| &diff.patch))
        } else {
            self.file_preview.as_ref().map(Result::as_ref)
        };
        let preview = content_preview.and_then(Result::ok);
        let modes: &[(DocumentMode, &str)] = if scope.is_some() {
            &[
                (DocumentMode::Unified, "Unified"),
                (DocumentMode::Patch, "Patch"),
                (DocumentMode::Preview, "Preview"),
                (DocumentMode::Raw, "Raw"),
            ]
        } else {
            &[
                (DocumentMode::Preview, "Preview"),
                (DocumentMode::Raw, "Raw"),
            ]
        };
        let editor = documents.editors.get(path);
        let draft = editor.and_then(Editor::draft);
        let raw = self.mode == DocumentMode::Raw;
        let copy = if raw {
            draft.map(|draft| ("Copy file", CopyContent::Draft(draft.input.clone())))
        } else {
            preview.and_then(|preview| preview.copy_content(viewing_diff))
        };
        let revert = review.and_then(|diff| diff.review.as_ref().zip(diff.revert.as_ref()));
        let truncated = matches!(
            preview,
            Some(Preview::Text {
                truncated: true,
                ..
            })
        );
        let content = if raw {
            editor
                .map_or_else(
                    || hint("Reading working file…", cx).p_4(),
                    |editor| editor.render(cx),
                )
                .into_any_element()
        } else {
            match content_preview {
                None => hint(
                    if viewing_diff {
                        "Reading diff…"
                    } else {
                        "Reading file…"
                    },
                    cx,
                )
                .p_4()
                .into_any_element(),
                Some(Err(error)) => hint(format!("Could not open {path_label}. {error:#}"), cx)
                    .p_4()
                    .into_any_element(),
                Some(Ok(preview)) => self.render_preview(preview, window, cx),
            }
        };
        v_flex()
            .id(("document", self.id))
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .flex_none()
                    .flex_wrap()
                    .px_3()
                    .py_1p5()
                    .gap_x_3()
                    .gap_y_1()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .min_w(px(120.))
                            .min_h_7()
                            .flex_1()
                            .gap_2()
                            .child(file_icons::icon(path, muted))
                            .child(control!(
                                format!("Document/{path_label}"),
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(crate::ui::control_text_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(foreground)
                                    .child(path_label)
                            )),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .max_w_full()
                            .flex_wrap()
                            .gap_1()
                            .child(self.mode_control(modes, cx))
                            .children(
                                copy.map(|(tooltip, content)| {
                                    self.copy_button(tooltip, content, cx)
                                }),
                            )
                            .when_some(
                                draft.filter(|draft| raw || draft.dirty),
                                |actions, draft| {
                                    actions.child(control!(
                                        "Save document",
                                        Button::new("save-document")
                                            .small()
                                            .label(if draft.is_saving() {
                                                "Saving…"
                                            } else {
                                                "Save"
                                            })
                                            .tooltip_with_action(
                                                "Save working file",
                                                &crate::SaveDocument,
                                                Some(DOCUMENT_CONTEXT),
                                            )
                                            .disabled(!draft.dirty || draft.is_saving())
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.save_active_document(
                                                    &crate::SaveDocument,
                                                    window,
                                                    cx,
                                                );
                                            }))
                                    ))
                                },
                            )
                            .when_some(revert, |actions, (row, target)| {
                                let changes = changes.clone();
                                let row = row.clone();
                                let target = target.clone();
                                actions.child(control!(
                                    "Revert document",
                                    Button::new("revert-document")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Undo2)
                                        .tooltip("Revert file")
                                        .disabled(
                                            self.diff_load.is_some()
                                                || draft.is_some_and(
                                                    |draft| draft.dirty || draft.is_saving()
                                                )
                                        )
                                        .on_click(move |_, window, cx| {
                                            changes.update(cx, |changes, cx| {
                                                changes.confirm_document_revert(
                                                    row.clone(),
                                                    target.clone(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        })
                                ))
                            }),
                    ),
            )
            .when_some(scope, |pane, scope| {
                let label = if self.mode == DocumentMode::Preview {
                    format!("{scope} · Working tree preview")
                } else {
                    scope.to_owned()
                };
                pane.child(
                    h_flex()
                        .px_3()
                        .py_1p5()
                        .gap_3()
                        .border_b_1()
                        .border_color(border)
                        .child(hint(label, cx).text_xs().flex_1())
                        .when_some(
                            review.and_then(|diff| diff.review.as_ref()),
                            |summary, row| summary.child(line_counts(row.file.lines, cx)),
                        ),
                )
            })
            .when_some(self.revert_error.as_ref(), |pane, error| {
                pane.child(control!(
                    "Document revert failed",
                    hint(error.clone(), cx).p_3().text_color(cx.theme().danger)
                ))
            })
            .when_some(
                draft.and_then(|draft| draft.error.as_ref()),
                |pane, error| {
                    pane.child(
                        h_flex()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .child(control!(
                                "Document save failed",
                                hint(error.clone(), cx)
                                    .flex_1()
                                    .text_color(cx.theme().danger)
                            ))
                            .child(control!(
                                "Reload document",
                                Button::new("reload-document")
                                    .ghost()
                                    .small()
                                    .label("Reload")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.reload_active_editor(window, cx)
                                    }))
                            )),
                    )
                },
            )
            .when(truncated && !raw, |pane| {
                pane.child(hint("Showing the first 128 KiB", cx).px_3().py_1())
            })
            .child(div().relative().flex_1().min_h_0().min_w_0().child(content))
            .into_any_element()
    }

    fn copy_button(
        &self,
        tooltip: &'static str,
        content: CopyContent,
        cx: &Context<Desktop>,
    ) -> Div {
        let id = self.id;
        let copied = self
            .copied_at
            .is_some_and(|at| at.elapsed() < COPY_FEEDBACK);
        control!(
            "Copy document",
            Button::new("copy-document")
                .ghost()
                .small()
                .icon(if copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .tooltip(tooltip)
                .disabled(match self.mode {
                    DocumentMode::Unified | DocumentMode::Patch => self.diff_load.is_some(),
                    DocumentMode::Preview => self.file_load.is_some(),
                    DocumentMode::Raw => false,
                })
                .on_click(cx.listener(move |desktop, _, _, cx| {
                    let Some(tab) = desktop.active_document_mut(id) else {
                        return;
                    };
                    cx.write_to_clipboard(content.clipboard_item(cx));
                    tab.copied_at = Some(Instant::now());
                    cx.notify();
                    cx.spawn(async |view, cx| {
                        cx.background_executor().timer(COPY_FEEDBACK).await;
                        let _ = view.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                }))
        )
    }

    fn mode_control(&self, modes: &[(DocumentMode, &str)], cx: &Context<Desktop>) -> Div {
        let id = self.id;
        h_flex()
            .flex_none()
            .rounded_md()
            .p_0p5()
            .bg(cx.theme().muted)
            .children(
                modes
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, (mode, label))| {
                        control!(
                            format!("Document mode/{label}"),
                            Button::new(("document-mode", index))
                                .ghost()
                                .xsmall()
                                .h_6()
                                .label(label.to_owned())
                                .selected(self.mode == mode)
                                .when(self.mode == mode, |button| button.bg(cx.theme().background))
                                .on_click(cx.listener(move |desktop, _, window, cx| {
                                    if let Some(thread) = &desktop.current
                                        && let Some(workspace) =
                                            desktop.workspaces.get_mut(&thread.id)
                                        && let Some(tab) = workspace.documents.active_mut(id)
                                    {
                                        tab.mode = mode;
                                        tab.copied_at = None;
                                        if mode == DocumentMode::Preview {
                                            tab.load_file_preview(&thread.id, &workspace.root, cx);
                                        }
                                        desktop.focus_workspace(window, cx);
                                        desktop.open_active_editor(window, cx);
                                        cx.notify();
                                    }
                                }))
                        )
                    }),
            )
    }

    fn render_preview(&self, preview: &Preview, window: &mut Window, cx: &mut App) -> AnyElement {
        let body = match preview {
            Preview::Notice(message) => {
                return control!(*message, hint(*message, cx).p_4()).into_any_element();
            }
            Preview::Image { thumbnail, .. } => div()
                .p_4()
                .child(control!("Rendered image", preview_bitmap(thumbnail))),
            #[cfg(target_os = "macos")]
            Preview::Pdf { pages, total } => v_flex()
                .w_full()
                .gap_4()
                .p_4()
                .when(pages.len() < *total, |body| {
                    body.child(hint(
                        format!("Showing {} of {total} pages", pages.len()),
                        cx,
                    ))
                })
                .children(pages.iter().enumerate().map(|(index, page)| {
                    let number = index + 1;
                    match page {
                        Some(page) => {
                            control!(format!("PDF page/{number}"), preview_bitmap(page).mx_auto())
                                .into_any_element()
                        }
                        None => {
                            hint(format!("Could not render page {number}."), cx).into_any_element()
                        }
                    }
                })),
            Preview::Text {
                formatted: Some(Formatted::Diff(diff)),
                ..
            } if self.mode == DocumentMode::Unified => {
                return Diff::render(diff, &self.diff_scroll, window, cx).into_any_element();
            }
            Preview::Text {
                text,
                formatted: Some(Formatted::Markdown(markdown)),
                ..
            } if self.mode == DocumentMode::Preview => div()
                .w_full()
                .px_6()
                .pb_10()
                .pt_4()
                .flex()
                .justify_center()
                .child(div().w_full().max_w(px(720.)).min_w_0().child(control!(
                    "Rendered Markdown",
                    markdown.render(
                        format!("markdown-document-{}", self.id),
                        text,
                        self.file_load
                            .is_none()
                            .then(|| self.pending_heading.take())
                            .flatten()
                            .as_deref()
                            .map(|heading| (heading, &self.scroll)),
                        window,
                        cx
                    )
                ))),
            Preview::Text { text, .. } => div()
                .min_w_full()
                .flex_none()
                .px_4()
                .py_3()
                .whitespace_nowrap()
                .font_family(cx.theme().mono_font_family.clone())
                .text_sm()
                .line_height(px(22.))
                .child(control!(
                    format!("Document text/{}", &text[..text.floor_char_boundary(1024)]),
                    StyledText::new(text.clone())
                )),
        };
        let scroll = if self.mode == DocumentMode::Patch {
            &self.patch_scroll
        } else {
            &self.scroll
        };
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("document-content")
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(scroll)
                    .child(body),
            )
            .scrollbar(scroll, ScrollbarAxis::Both)
            .into_any_element()
    }
}

fn preview_bitmap(image: &Arc<RenderImage>) -> Div {
    let size = image.size(0);
    let width = size.width.0 as f32;
    let height = size.height.0 as f32;
    let mut frame = div()
        .relative()
        .w_full()
        .max_w(px(width.min(840.)))
        .child(img(image.clone()).absolute().size_full());
    frame.style().aspect_ratio = Some(width / height);
    frame
}

fn reveal_tab(reveal: &Cell<Option<usize>>, scroll: &ScrollHandle, window: &mut Window) {
    let Some(tab) = reveal
        .take()
        .and_then(|index| scroll.bounds_for_item(index))
    else {
        return;
    };
    let viewport = scroll.bounds();
    let offset = scroll.offset();
    let right = viewport.right() - tab.right();
    let left = (viewport.left() - tab.left()).min(right);
    let x = offset.x.clamp(left, right);
    if x != offset.x {
        scroll.set_offset(point(x, offset.y));
        window.refresh();
    }
}
