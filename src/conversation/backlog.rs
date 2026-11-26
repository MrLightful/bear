use super::*;
use anyhow::{Context as _, ensure};
use huggler::Attachment;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

/// A snapshot of the launch form. Skill and file mentions are part of the message.
#[derive(Serialize, Deserialize)]
pub(super) struct LaunchDraft {
    pub name: String,
    pub message: String,
    pub checkout: PathBuf,
    #[serde(default)]
    pub checkout_root: Option<PathBuf>,
    pub project_name: Option<String>,
    pub workspace_mode: WorkspaceMode,
    pub selected_branch: Option<git::Branch>,
    pub branch: String,
    pub model: Option<String>,
    pub model_options: ModelOptions,
    pub instance: ProviderInstanceId,
}

#[derive(Serialize, Deserialize)]
struct SavedDraft {
    launch: LaunchDraft,
    files: Vec<SavedFile>,
    created: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, Deserialize)]
struct SavedFile {
    name: String,
    mime: String,
    size: usize,
}

#[derive(Default)]
pub(super) struct Backlog {
    pub open: bool,
    pub sending: Option<Uuid>,
    entries: Vec<(Uuid, SavedDraft)>,
    pub errors: HashMap<Uuid, DraftError>,
    edit: Option<DraftEdit>,
    expanded_details: HashSet<Uuid>,
}

pub(super) enum DraftError {
    Delivery(String),
    Edit(String),
}

impl DraftError {
    fn message(&self) -> &str {
        match self {
            Self::Delivery(message) | Self::Edit(message) => message,
        }
    }
}

struct DraftEdit {
    id: Uuid,
    name: Entity<InputState>,
    message: Entity<InputState>,
}

impl SavedDraft {
    fn write(
        path: &Path,
        launch: LaunchDraft,
        files: &[Attachment],
        created: chrono::DateTime<chrono::Utc>,
        replace: bool,
    ) -> anyhow::Result<()> {
        let draft = Self {
            launch,
            files: files
                .iter()
                .map(|file| SavedFile {
                    name: file.name.clone(),
                    mime: file.mime.clone(),
                    size: file.bytes.len(),
                })
                .collect(),
            created,
        };
        let directory = path.parent().expect("draft directory");
        fs::create_dir_all(directory)?;
        let mut pending = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut pending, &draft)?;
        pending.write_all(b"\n")?;
        for file in files {
            pending.write_all(&file.bytes)?;
        }
        pending.as_file().sync_all()?;
        if replace {
            pending.persist(path)?;
        } else {
            pending.persist_noclobber(path)?;
        }
        Ok(())
    }

    // A JSON metadata line followed by the original file bytes lets the page
    // list and display drafts without opening attachment bodies.
    fn read_header(reader: &mut impl BufRead) -> anyhow::Result<Self> {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        Ok(serde_json::from_str(&header)?)
    }

    fn read(path: &Path) -> anyhow::Result<(LaunchDraft, Attachments)> {
        let mut reader = BufReader::new(fs::File::open(path)?);
        let draft = Self::read_header(&mut reader)?;
        let mut files = Vec::with_capacity(draft.files.len());
        let mut remaining = Attachment::MAX_TOTAL_BYTES;
        for file in draft.files {
            ensure!(
                file.size <= remaining,
                "Draft attachments exceed the upload limit"
            );
            remaining -= file.size;
            let mut bytes = vec![0; file.size];
            reader.read_exact(&mut bytes)?;
            files.push(Attachment {
                name: file.name,
                mime: file.mime,
                bytes,
            });
        }
        let attachments = Attachments::from_files(&draft.launch.message, files);
        Ok((draft.launch, attachments))
    }
}

// The checkout query returns Forbidden when the thread has no checkout record.
pub(super) fn has_draft_conversation(runtime: &Huggler, id: &ThreadId) -> huggler::Result<bool> {
    match runtime.thread_checkout(id) {
        Ok(_) => Ok(true),
        Err(huggler::Error::Forbidden) => Ok(false),
        Err(error) => Err(error),
    }
}

impl Desktop {
    fn backlog_dir(&self) -> PathBuf {
        self.selection_file.with_file_name("backlog")
    }

    fn backlog_path(&self, id: Uuid) -> PathBuf {
        self.backlog_dir()
            .join(id.to_string())
            .with_extension("draft")
    }

    fn load_backlog(&mut self) -> anyhow::Result<()> {
        self.backlog.entries.clear();
        let directory = match fs::read_dir(self.backlog_dir()) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut failed = 0;
        for entry in directory {
            let Ok(entry) = entry else {
                failed += 1;
                continue;
            };
            let path = entry.path();
            let Some(id) = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| Uuid::parse_str(name).ok())
                .filter(|_| {
                    path.extension()
                        .is_some_and(|extension| extension == "draft")
                })
            else {
                continue;
            };
            match fs::File::open(path)
                .map(BufReader::new)
                .map_err(anyhow::Error::from)
                .and_then(|mut reader| SavedDraft::read_header(&mut reader))
            {
                Ok(draft) => self.backlog.entries.push((id, draft)),
                Err(_) => failed += 1,
            }
        }
        self.backlog
            .entries
            .sort_unstable_by_key(|(_, draft)| std::cmp::Reverse(draft.created));
        ensure!(failed == 0, "Could not read {failed} saved draft(s)");
        Ok(())
    }

    pub(super) fn open_backlog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_settings(cx) {
            return;
        }
        self.suggestions = Default::default();
        self.project_picker = None;
        self.model_picker = None;
        self.mcp_status = None;
        self.launch_workspace.close_picker();
        #[cfg(unix)]
        {
            self.remote_project = None;
        }
        if !self.backlog.open {
            self.backlog.expanded_details.clear();
        }
        self.backlog.open = true;
        self.error = self
            .load_backlog()
            .err()
            .map(|error| format!("Could not load Backlog. {error:#}"));
        self.focus.focus(window);
        cx.notify();
    }

    fn save_to_backlog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if self.launching.is_some()
            || self.checkout_browse.is_some()
            || self.starter_attachments.loading
            || self.launch_workspace.loading_branches()
        {
            return Ok(());
        }
        let mut launch = self.launch_draft(cx);
        // Save the branch shown in the form even when it was chosen by default.
        if !launch.checkout.as_os_str().is_empty() {
            launch.checkout = std::path::absolute(&launch.checkout)?;
            launch.checkout_root = self
                .launch_workspace
                .branches()
                .map(|branches| branches.root.canonicalize())
                .transpose()?;
            launch.workspace_mode = self.launch_workspace.mode();
            launch.selected_branch = self
                .launch_workspace
                .displayed_branch(launch.workspace_mode)
                .cloned();
        }
        let (message, files) = self.starter_attachments.content(&launch.message);
        launch.message = message;
        ensure!(
            !launch.message.trim().is_empty() || !files.is_empty(),
            "Add a message or attachment first"
        );
        SavedDraft::write(
            &self.backlog_path(Uuid::new_v4()),
            launch,
            &files,
            chrono::Utc::now(),
            false,
        )?;
        self.starter_attachments.take();
        self.starter
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.branch
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.open_backlog(window, cx);
        Ok(())
    }

    pub(super) fn save_backlog_button(&self, disabled: bool, cx: &Context<Self>) -> Div {
        control!(
            "Save to Backlog",
            Button::new("save-to-backlog")
                .small()
                .ghost()
                .icon(IconName::GalleryVerticalEnd)
                .label("Save to Backlog")
                .disabled(disabled || self.launch_workspace.loading_branches())
                .on_click(cx.listener(|view, _, window, cx| {
                    if let Err(error) = view.save_to_backlog(window, cx) {
                        view.error = Some(format!("Could not save draft. {error:#}"));
                        cx.notify();
                    }
                }))
        )
        .flex_shrink_0()
    }

    fn send_backlog(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.backlog.sending.is_some()
            || self.launching.is_some()
            || self.backlog.edit.as_ref().is_some_and(|edit| edit.id == id)
        {
            return;
        }
        self.backlog.errors.remove(&id);
        match SavedDraft::read(&self.backlog_path(id)).context("Could not read the saved draft") {
            Ok((draft, attachments)) => self.start_launch(draft, attachments, Some(id), window, cx),
            Err(error) => {
                self.backlog
                    .errors
                    .insert(id, DraftError::Delivery(format!("{error:#}")));
                cx.notify();
            }
        }
    }

    pub(super) fn remove_saved_draft(&mut self, id: Uuid) -> std::io::Result<()> {
        match fs::remove_file(self.backlog_path(id)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        self.backlog.entries.retain(|(entry, _)| *entry != id);
        self.backlog.errors.remove(&id);
        self.backlog.expanded_details.remove(&id);
        if self.backlog.edit.as_ref().is_some_and(|edit| edit.id == id) {
            self.backlog.edit = None;
        }
        Ok(())
    }

    fn start_draft_edit(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.backlog.sending == Some(id) || self.backlog.edit.is_some() {
            return;
        }
        match has_draft_conversation(&self.runtime, &ThreadId(id.to_string())) {
            Ok(false) => (),
            Ok(true) => {
                self.backlog.errors.insert(
                    id,
                    DraftError::Delivery(
                        "This draft already has a conversation. Retry sending the saved message."
                            .into(),
                    ),
                );
                cx.notify();
                return;
            }
            Err(error) => {
                self.backlog
                    .errors
                    .insert(id, DraftError::Edit(error.to_string()));
                cx.notify();
                return;
            }
        }
        let Some((_, draft)) = self.backlog.entries.iter().find(|(entry, _)| *entry == id) else {
            return;
        };
        let name =
            cx.new(|cx| InputState::new(window, cx).default_value(draft.launch.name.clone()));
        let message = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(3, 12)
                .default_value(draft.launch.message.clone())
        });
        cx.observe(&message, |_, _, cx| cx.notify()).detach();
        message.focus_handle(cx).focus(window);
        self.backlog.edit = Some(DraftEdit { id, name, message });
        self.backlog.errors.remove(&id);
        cx.notify();
    }

    fn save_draft_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let Some(edit) = &self.backlog.edit else {
            return Ok(());
        };
        let id = edit.id;
        let name = edit.name.read(cx).value().trim().to_owned();
        let message = edit.message.read(cx).value().to_string();
        let (_, draft) = self
            .backlog
            .entries
            .iter()
            .find(|(entry, _)| *entry == id)
            .context("Draft is no longer available")?;
        let path = self.backlog_path(id);
        let (mut launch, attachments) = SavedDraft::read(&path)?;
        let message = attachments.restore_references(message);
        ensure!(
            !message.trim().is_empty() || !attachments.is_empty(&message),
            "Add a message or keep an attachment"
        );
        launch.name = if name.is_empty() {
            attachments.title(&message)
        } else {
            name
        };
        let (message, files) = attachments.content(&message);
        launch.message = message;
        SavedDraft::write(&path, launch, &files, draft.created, true)?;
        self.backlog.edit = None;
        self.open_backlog(window, cx);
        Ok(())
    }

    fn confirm_delete_draft(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.backlog.sending == Some(id) {
            return;
        }
        #[cfg(all(feature = "qualification", target_os = "macos"))]
        let scripted = crate::qualification::take_prompt_answer(cx);
        #[cfg(not(all(feature = "qualification", target_os = "macos")))]
        let scripted: Option<usize> = None;
        let prompt = scripted.is_none().then(|| {
            window.prompt(
                PromptLevel::Warning,
                "Delete this draft?",
                Some("This removes the saved message and its attachments from Backlog."),
                &["Cancel", "Delete"],
                cx,
            )
        });
        cx.spawn(async move |view, cx| {
            let answer = match prompt {
                Some(prompt) => prompt.await.ok(),
                None => scripted,
            };
            if answer == Some(1) {
                let _ = view.update(cx, |view, cx| {
                    if view.backlog.sending != Some(id)
                        && let Err(error) = view.remove_saved_draft(id)
                    {
                        view.error = Some(format!("Could not delete draft. {error}"));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn draft_editor(&self, edit: &DraftEdit, cx: &Context<Self>) -> Div {
        let id = edit.id;
        let can_save = !edit.message.read(cx).value().trim().is_empty()
            || self
                .backlog
                .entries
                .iter()
                .any(|(entry, draft)| *entry == id && !draft.files.is_empty());
        v_flex()
            .gap_3()
            .child(field("Draft message", &edit.message))
            .child(field("Conversation title (optional)", &edit.name))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(control!(
                        "Cancel draft edit",
                        Button::new("cancel-draft-edit")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.backlog.edit = None;
                                view.backlog.errors.remove(&id);
                                view.focus.focus(window);
                                cx.notify();
                            }))
                    ))
                    .child(control!(
                        "Save draft changes",
                        Button::new("save-draft-edits")
                            .small()
                            .primary()
                            .label("Save changes")
                            .disabled(!can_save)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if let Err(error) = view.save_draft_edit(window, cx) {
                                    view.backlog.errors.insert(
                                        id,
                                        DraftError::Edit(format!(
                                            "Could not save changes. {error:#}"
                                        )),
                                    );
                                    cx.notify();
                                }
                            }))
                    )),
            )
    }

    pub(super) fn backlog_page(
        &self,
        navigation_open: bool,
        width: Pixels,
        cx: &Context<Self>,
    ) -> Div {
        let count = self.backlog.entries.len();
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h_10()
                    .flex_shrink_0()
                    .px_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .gap_2()
                    .child(self.navigation_toggle(navigation_open, cx))
                    .child(label("Backlog")),
            )
            .child(
                v_flex()
                    .id("backlog-list")
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            // Measure wrapped rows against the pane, including inside the scroller.
                            .w(width.min(px(768.)))
                            .max_w_full()
                            .flex_shrink_0()
                            .px_4()
                            .pt_6()
                            .pb_8()
                            .gap_4()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .min_w_0()
                                            .child(heading("Saved drafts").text_xl())
                                            .when(count > 0, |title| {
                                                title.child(
                                                    hint(count.to_string(), cx)
                                                        .px_2()
                                                        .py_0p5()
                                                        .rounded_full()
                                                        .bg(cx.theme().muted),
                                                )
                                            }),
                                    )
                                    .when(count > 0, |header| {
                                        header.child(self.new_draft_button(false, cx))
                                    }),
                            )
                            .child(
                                v_flex()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().muted.opacity(0.25))
                                    .when(count == 0, |list| list.child(self.empty_backlog(cx)))
                                    .children(self.backlog.entries.iter().enumerate().map(
                                        |(index, (id, draft))| {
                                            self.backlog_row(*id, draft, cx).when(
                                                index > 0,
                                                |row| {
                                                    row.border_t_1().border_color(cx.theme().border)
                                                },
                                            )
                                        },
                                    )),
                            ),
                    ),
            )
    }

    fn new_draft_button(&self, empty: bool, cx: &Context<Self>) -> Div {
        control!(
            "Create a draft",
            Button::new("create-draft")
                .small()
                .when(empty, |button| button.primary())
                .when(!empty, |button| button.outline())
                .icon(IconName::Plus)
                .label("New draft")
                .on_click(
                    cx.listener(|view, _, window, cx| { view.new_conversation(false, window, cx) })
                )
        )
        .flex_shrink_0()
    }

    fn empty_backlog(&self, cx: &Context<Self>) -> Div {
        v_flex()
            .items_center()
            .text_center()
            .px_5()
            .py_10()
            .gap_4()
            .child(
                h_flex()
                    .size_10()
                    .justify_center()
                    .rounded_lg()
                    .bg(cx.theme().muted)
                    .child(
                        Icon::new(IconName::GalleryVerticalEnd)
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(heading("No drafts yet"))
                    .child(
                        hint(
                            "Use Save to Backlog in the composer to keep a message for later.",
                            cx,
                        )
                        .max_w(px(280.)),
                    ),
            )
            .child(self.new_draft_button(true, cx))
    }

    fn draft_details(
        &self,
        id: Uuid,
        draft: &SavedDraft,
        message: &str,
        editing: bool,
        cx: &Context<Self>,
    ) -> Div {
        let launch = &draft.launch;
        let branch = if launch.checkout.as_os_str().is_empty() {
            None
        } else {
            match launch.workspace_mode {
                WorkspaceMode::Current => launch.selected_branch.as_ref().map(|b| b.name.clone()),
                WorkspaceMode::Worktree => Some(
                    launch
                        .selected_branch
                        .as_ref()
                        .map_or_else(|| "Repository default".into(), |branch| branch.name.clone()),
                ),
            }
        };
        let provider = self
            .runtime
            .output()
            .provider_label(launch.instance.as_str());
        let model = launch.model.as_deref().unwrap_or("Automatic");
        let mut model_options = Vec::new();
        if let Some(effort) = &launch.model_options.effort {
            model_options.push(format!("{} effort", models::effort_label(effort)));
        }
        if let Some(tier) = &launch.model_options.service_tier {
            model_options.push(match tier.as_str() {
                "priority" | "fast" => "Fast mode".into(),
                "default" => "Standard speed".into(),
                _ => format!("Service tier {tier}"),
            });
        }
        v_flex()
            .gap_4()
            .mx_4()
            .pb_4()
            .pt_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .when(!editing && !message.trim().is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .gap_1()
                        .child(crate::ui::section_title("Message"))
                        .child(draft_prompt_block(message.to_owned(), cx)),
                )
            })
            .when(!draft.files.is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .gap_1()
                        .child(crate::ui::section_title("Attachments"))
                        .children(draft.files.iter().map(|file| {
                            h_flex()
                                .items_start()
                                .gap_2()
                                .child(crate::file_icons::icon(
                                    Path::new(&file.name),
                                    cx.theme().muted_foreground,
                                ))
                                .child(hint(file.name.clone(), cx).min_w_0())
                        })),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(crate::ui::section_title("Launch settings").mb_1())
                    .when(!editing && !launch.name.trim().is_empty(), |details| {
                        details.child(control!(
                            format!("Draft title/{}", launch.name),
                            draft_option("Title", launch.name.clone(), cx)
                        ))
                    })
                    .when(!launch.checkout.as_os_str().is_empty(), |details| {
                        details.child(draft_option(
                            "Checkout",
                            launch.checkout.display().to_string(),
                            cx,
                        ))
                    })
                    .when_some(branch, |details, branch| {
                        let label = match launch.workspace_mode {
                            WorkspaceMode::Worktree => "Base branch",
                            WorkspaceMode::Current => "Branch",
                        };
                        details.child(draft_option(label, branch, cx))
                    })
                    .when(
                        launch.workspace_mode == WorkspaceMode::Worktree
                            && !launch.checkout.as_os_str().is_empty()
                            && !launch.branch.is_empty(),
                        |details| {
                            details.child(draft_option("New branch", launch.branch.clone(), cx))
                        },
                    )
                    .child(draft_option("Model", format!("{provider} · {model}"), cx))
                    .when(!model_options.is_empty(), |details| {
                        details.child(draft_option("Options", model_options.join(" · "), cx))
                    }),
            )
            .when(!editing, |panel| {
                panel.child(h_flex().justify_end().child(control!(
                        format!("Delete draft/{}", launch.name),
                        Button::new((ElementId::from(id), "delete"))
                            .small()
                            .ghost()
                            .label("Delete draft")
                            .disabled(self.backlog.sending == Some(id))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.confirm_delete_draft(id, window, cx)
                            }))
                    )))
            })
    }

    fn backlog_row(&self, id: Uuid, draft: &SavedDraft, cx: &Context<Self>) -> Div {
        let launch = &draft.launch;
        let sending = self.backlog.sending == Some(id);
        let error = self.backlog.errors.get(&id);
        let action = if sending {
            "Starting…"
        } else if matches!(error, Some(DraftError::Delivery(_))) {
            "Retry"
        } else {
            "Start"
        };
        let expanded = self.backlog.expanded_details.contains(&id);
        let edit = self.backlog.edit.as_ref().filter(|edit| edit.id == id);
        let message = attachments::saved_caption(
            &launch.message,
            draft.files.iter().map(|file| file.name.as_str()),
        );
        let preview = message.split_whitespace().collect::<Vec<_>>().join(" ");
        let date = draft
            .created
            .with_timezone(&chrono::Local)
            .format("%b %-d, %Y");
        let attachment_count = match draft.files.len() {
            0 => None,
            1 => Some("1 attachment".into()),
            count => Some(format!("{count} attachments")),
        };
        let workspace = if launch.checkout.as_os_str().is_empty() {
            "Scratch workspace".into()
        } else {
            let project = launch
                .project_name
                .clone()
                .or_else(|| {
                    launch
                        .checkout
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "Git checkout".into());
            format!("{project} · {}", launch.workspace_mode.label())
        };
        let metadata = h_flex()
            .flex_wrap()
            .justify_between()
            .gap_2()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .min_w_0()
                    .max_w_full()
                    .child(draft_meta(
                        if launch.checkout.as_os_str().is_empty() {
                            IconName::SquareTerminal
                        } else {
                            IconName::Folder
                        },
                        workspace,
                        cx,
                    ))
                    .when_some(attachment_count, |metadata, count| {
                        metadata.child(draft_meta(IconName::File, count, cx))
                    }),
            )
            .child(hint(date.to_string(), cx).text_xs().whitespace_nowrap());
        let actions = h_flex()
            .flex_wrap()
            .justify_between()
            .gap_2()
            .pt_2()
            .child(control!(
                format!("Draft details/{}", launch.name),
                Button::new((ElementId::from(id), "details"))
                    .small()
                    .compact()
                    .ghost()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(if expanded {
                        "Hide details"
                    } else {
                        "View draft"
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if !view.backlog.expanded_details.remove(&id) {
                            view.backlog.expanded_details.insert(id);
                        }
                        cx.notify();
                    }))
            ))
            .when(edit.is_none(), |actions| {
                actions.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .max_w_full()
                        .child(control!(
                            format!("Edit draft/{}", launch.name),
                            Button::new((ElementId::from(id), "edit"))
                                .small()
                                .compact()
                                .ghost()
                                .label("Edit")
                                .disabled(sending || self.backlog.edit.is_some())
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.start_draft_edit(id, window, cx)
                                }))
                        ))
                        .child(control!(
                            format!("Send draft/{}", launch.name),
                            Button::new((ElementId::from(id), "send"))
                                .small()
                                .primary()
                                .icon(IconName::ArrowRight)
                                .label(action)
                                .loading(sending)
                                .disabled(
                                    self.backlog.sending.is_some() || self.launching.is_some()
                                )
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.send_backlog(id, window, cx)
                                }))
                        )),
                )
            });
        let row = v_flex()
            .id(id)
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .when(edit.is_none(), |body| {
                        body.child(control!(
                            format!("Draft prompt/{}", launch.name),
                            draft_prompt_block(
                                if preview.is_empty() {
                                    "Attachment-only draft".into()
                                } else {
                                    preview
                                },
                                cx
                            )
                            .line_clamp(3)
                        ))
                    })
                    .when_some(edit, |body, edit| body.child(self.draft_editor(edit, cx)))
                    .child(metadata)
                    .child(actions)
                    .when_some(error, |body, error| {
                        body.child(control!(
                            format!("error/{}", error.message()),
                            h_flex()
                                .items_start()
                                .gap_2()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().danger.opacity(0.10))
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .small()
                                        .flex_shrink_0()
                                        .text_color(
                                            cx.theme()
                                                .danger
                                                .blend(cx.theme().foreground.opacity(0.60))
                                        ),
                                )
                                .child(div().min_w_0().text_sm().child(error.message().to_owned()))
                        ))
                    }),
            )
            .when(expanded, |row| {
                row.child(control!(
                    format!("Draft details panel/{}", launch.name),
                    self.draft_details(id, draft, &message, edit.is_some(), cx)
                ))
            });
        control!(format!("Draft card/{}", launch.name), row)
    }
}

fn draft_prompt_block(text: String, cx: &App) -> Div {
    div()
        .w_full()
        .rounded_md()
        .bg(cx.theme().muted)
        .px_3()
        .py_2()
        .text_sm()
        .font_weight(FontWeight::NORMAL)
        .line_height(rems(1.375))
        .child(text)
}

fn draft_meta(icon: IconName, text: String, cx: &App) -> Div {
    h_flex()
        .min_w_0()
        .max_w_full()
        .gap_1p5()
        .child(
            Icon::new(icon)
                .with_size(px(13.))
                .text_color(cx.theme().muted_foreground),
        )
        .child(hint(text, cx).min_w_0().truncate())
}

fn draft_option(name: &'static str, value: String, cx: &App) -> Div {
    h_flex()
        .items_start()
        .gap_2()
        .child(
            label(name)
                .w(rems(5.75))
                .flex_shrink_0()
                .whitespace_nowrap(),
        )
        .child(hint(value, cx).min_w_0())
}
