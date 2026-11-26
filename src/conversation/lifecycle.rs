use super::*;
use gpui_component::{
    list::{List, ListEvent},
    popover::Popover,
};
use launch_workspace::BranchPicker;

impl Desktop {
    pub(super) fn history_navigation(&self, cx: &Context<Self>) -> Div {
        control!(
            "History",
            Button::new("conversation-history")
                .small()
                .compact()
                .w_full()
                .justify_start()
                .ghost()
                .icon(IconName::BookOpen)
                .label("History")
                .selected(self.history_open && self.local_workspace_visible())
                .tooltip(if self.history_open {
                    "Show active conversations"
                } else {
                    "Show settled conversations"
                })
                .on_click(cx.listener(|view, _, window, cx| {
                    let was_showing_history = view.history_open && view.local_workspace_visible();
                    if !view.leave_settings(cx) {
                        return;
                    }
                    view.backlog.open = false;
                    #[cfg(unix)]
                    {
                        view.remote_project = None;
                    }
                    view.history_open = !was_showing_history;
                    if view.history_open {
                        view.history_search.focus_handle(cx).focus(window);
                    } else {
                        view.focus_workspace(window, cx);
                    }
                    cx.notify();
                }))
        )
    }

    pub(super) fn history_sidebar(&self, width: f32, cx: &Context<Self>) -> Div {
        let query = self.history_search.read(cx).value().trim().to_lowercase();
        let rows: Vec<_> = self
            .threads
            .iter()
            .filter(|thread| thread.settled && thread.name.to_lowercase().contains(&query))
            .collect();
        let empty = if query.is_empty() {
            "No settled conversations"
        } else {
            "No matching conversations"
        };
        v_flex()
            .gap_2()
            .child(control!(
                "Search conversations",
                Input::new(&self.history_search).small()
            ))
            .when(rows.is_empty(), |history| history.child(hint(empty, cx)))
            .children(rows.into_iter().map(|thread| {
                self.sidebar_row(thread, sidebar::Section::Settled, "Settled", width, cx)
            }))
    }

    pub(super) fn open_worktrees(&self, _cx: &App) -> HashSet<PathBuf> {
        self.workspaces
            .values()
            .filter(|workspace| {
                #[cfg(target_os = "macos")]
                if workspace.terminal.has_running_terminal(_cx) {
                    return true;
                }
                workspace.documents.has_unsaved_changes()
            })
            .map(|workspace| workspace.root.clone())
            .collect()
    }

    pub(super) fn update_checkout_availability(&mut self, thread: &ThreadId) {
        let path = &self.workspaces[thread].root;
        self.checkout_unavailable = !path.is_dir();
        self.released_source = if self.runtime.worktree_released(path).unwrap_or(false) {
            Some(self.runtime.released_worktree_source(path))
        } else if self.checkout_unavailable {
            match self.runtime.released_worktree_source(path) {
                Err(huggler::Error::Forbidden) => None,
                result => Some(result),
            }
        } else {
            None
        };
    }

    pub(super) fn settled_footer(&self, thread: &ConversationDetail, cx: &Context<Self>) -> Div {
        let id = thread.id.clone();
        let delete_id = id.clone();
        let needs_branch = self.checkout_unavailable && self.released_source.is_some();
        let restore_message =
            self.released_source
                .as_ref()
                .filter(|_| needs_branch)
                .map(|source| match source {
                    Ok(_) => {
                        "Worktree removed. Choose a branch to reopen this conversation.".to_owned()
                    }
                    Err(error) => format!("The original repository is unavailable. {error}"),
                });
        v_flex()
            .mx_4()
            .mb_2()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_2()
            .child(hint(
                restore_message.unwrap_or_else(|| "Settled conversation".into()),
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when(!needs_branch, |actions| {
                        actions.child(control!(
                            "Reopen conversation",
                            Button::new("reopen-conversation")
                                .small()
                                .label("Reopen conversation")
                                .disabled(self.restoring_worktree)
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    if view.released_source.is_some() {
                                        let path = view.workspaces[&id].root.clone();
                                        view.restore_worktree(
                                            id.clone(),
                                            path,
                                            "HEAD".into(),
                                            window,
                                            cx,
                                        );
                                    } else {
                                        view.reopen_conversation(id.clone(), window, cx);
                                    }
                                }))
                        ))
                    })
                    .when_some(
                        self.released_source
                            .as_ref()
                            .and_then(|source| source.as_ref().ok())
                            .filter(|_| needs_branch),
                        |actions, source| {
                            actions.child(self.restore_worktree_control(&thread.id, source, cx))
                        },
                    )
                    .child(control!(
                        "Delete conversation",
                        Button::new("delete-conversation")
                            .small()
                            .ghost()
                            .label("Delete conversation")
                            .disabled(self.restoring_worktree)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.confirm_delete_conversation(delete_id.clone(), window, cx);
                            }))
                    )),
            )
    }

    fn restore_worktree_control(
        &self,
        thread: &ThreadId,
        source: &std::path::Path,
        cx: &Context<Self>,
    ) -> Div {
        let source = source.to_owned();
        let thread = thread.clone();
        let label = if self.restoring_worktree {
            "Restoring…"
        } else {
            "Choose branch"
        };
        let keyboard_source = source.clone();
        let keyboard_thread = thread.clone();
        let trigger = Button::new("reopen-with-worktree")
            .small()
            .label(label)
            .dropdown_caret(true)
            .disabled(self.restoring_worktree)
            .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                if event.is_keyboard() {
                    if view.restore_branch_picker.is_some() {
                        view.close_restore_picker(cx);
                    } else {
                        view.load_restore_branches(
                            keyboard_thread.clone(),
                            keyboard_source.clone(),
                            window,
                            cx,
                        );
                    }
                }
            }));
        if self.restoring_worktree {
            return control!("Choose branch to reopen", trigger);
        }
        let picker = Popover::new("restore-branch-picker")
            .anchor(Corner::BottomLeft)
            .trigger(trigger)
            .open(self.restore_branch_picker.is_some())
            .on_open_change(cx.listener(move |view, open, window, cx| {
                if *open {
                    view.load_restore_branches(thread.clone(), source.clone(), window, cx);
                } else {
                    view.close_restore_picker(cx);
                    view.focus_workspace(window, cx);
                }
            }));
        let picker = if let Some(list) = &self.restore_branch_picker {
            picker.child(
                v_flex().w(px(284.)).child(
                    List::new(list)
                        .search_placeholder("Search branches")
                        .h(px(196.)),
                ),
            )
        } else {
            picker
        };
        control!("Choose branch to reopen", picker)
    }

    pub(super) fn close_restore_picker(&mut self, cx: &mut Context<Self>) {
        self.restore_branch_load = None;
        if self.restore_branch_picker.take().is_some() {
            cx.notify();
        }
    }

    fn load_restore_branches(
        &mut self,
        thread: ThreadId,
        source: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = source.clone();
        let job = cx.background_spawn(async move { git::branches(&root) });
        self.restore_branch_load = Some(cx.spawn_in(window, async move |view, cx| {
            let result = job.await;
            let _ = view.update_in(cx, |view, window, cx| {
                let branches = match result {
                    Ok(branches) => branches,
                    Err(error) => {
                        view.error = Some(format!("Could not list branches. {error:#}"));
                        cx.notify();
                        return;
                    }
                };
                let list = cx.new(|cx| {
                    ListState::new(BranchPicker::new(branches.branches, None), window, cx)
                        .searchable(true)
                });
                cx.subscribe_in(&list, window, move |view, list, event, window, cx| {
                    if view.restore_branch_picker.as_ref() != Some(list) {
                        return;
                    }
                    match event {
                        ListEvent::Confirm(index) => {
                            let Some(branch) = list.read(cx).delegate().branch(*index) else {
                                return;
                            };
                            let revision = branch.reference.clone();
                            view.close_restore_picker(cx);
                            view.restore_worktree(
                                thread.clone(),
                                source.clone(),
                                revision,
                                window,
                                cx,
                            );
                        }
                        ListEvent::Cancel => {
                            view.close_restore_picker(cx);
                            view.focus_workspace(window, cx);
                        }
                        ListEvent::Select(_) => (),
                    }
                })
                .detach();
                list.focus_handle(cx).focus(window);
                view.restore_branch_picker = Some(list);
                cx.notify();
            });
        }));
    }

    fn restore_worktree(
        &mut self,
        id: ThreadId,
        source: PathBuf,
        revision: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_worktree {
            return;
        }
        self.restoring_worktree = true;
        self.error = None;
        let path = self.workspaces[&id].root.clone();
        let runtime = self.runtime.clone();
        let job = cx.background_spawn(async move {
            runtime.restore_managed_worktree(&path, &source, &revision)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = job.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.restoring_worktree = false;
                if let Err(error) = result {
                    view.report(Err(error), cx);
                    return;
                }
                let selected = view.current.as_ref().is_some_and(|thread| thread.id == id);
                if selected {
                    view.update_checkout_availability(&id);
                    view.workspaces[&id].refresh_after_checkout_update(view.workspace_tab, cx);
                    view.resolve_main_worktree(id.clone(), cx);
                    view.watch_pull_request(cx);
                }
                view.reopen_conversation(id, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn reopen_conversation(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        let result = self
            .runtime
            .dispatch(Command {
                id: CommandId(Uuid::new_v4().to_string()),
                thread_id: id.clone(),
                action: Action::SetSettled { settled: false },
            })
            .map(|_| ());
        let result = result.and_then(|_| {
            if self.current.as_ref().is_some_and(|thread| thread.id == id) {
                self.refresh()?;
                self.history_open = false;
                self.preload_models(cx);
                self.prompt.focus_handle(cx).focus(window);
            }
            Ok(())
        });
        self.report(result, cx);
    }

    fn confirm_delete_conversation(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(all(feature = "qualification", target_os = "macos"))]
        let scripted = crate::qualification::take_prompt_answer(cx);
        #[cfg(not(all(feature = "qualification", target_os = "macos")))]
        let scripted: Option<usize> = None;
        let prompt = scripted.is_none().then(|| {
            window.prompt(
                PromptLevel::Warning,
                "Delete this conversation?",
                Some("Its messages and attachments will be permanently deleted. Bear will also check whether it can remove its worktree."),
                &["Cancel", "Delete"],
                cx,
            )
        });
        cx.spawn_in(window, async move |view, cx| {
            let answer = match prompt {
                Some(prompt) => prompt.await.ok(),
                None => scripted,
            };
            if answer != Some(1) {
                return;
            }
            let _ = view.update_in(cx, |view, window, cx| {
                if view.workspaces.get(&id).is_some_and(|workspace| workspace.documents.has_unsaved_changes()) {
                    view.report(Err(huggler::Error::Invalid("Save or discard the open file edits before deleting this conversation.".into())), cx);
                    return;
                }
                let path = view.workspaces.get(&id).map(|workspace| workspace.root.clone());
                let protected = view.open_worktrees(cx);
                let result = view.runtime.delete_thread(&id);
                if result.is_ok() {
                    if view.current.as_ref().is_some_and(|thread| thread.id == id) {
                        view.new_conversation(false, window, cx);
                        view.history_open = true;
                        let _ = fs::remove_file(&view.selection_file);
                    }
                    if let Some(_workspace) = view.workspaces.remove(&id) {
                        #[cfg(target_os = "macos")]
                        {
                            let mut workspace = _workspace;
                            workspace.terminal.release(cx);
                        }
                    }
                }
                if result.is_ok() && let Some(path) = path {
                    let runtime = view.runtime.clone();
                    let prefs = view.settings.read(cx).preferences().clone();
                    let executor = view.executor.clone();
                    let job = cx.background_spawn(async move {
                        executor.block_on(crate::settings::reclaim_deleted_checkout(runtime, &prefs, &protected, &path))
                    });
                    cx.spawn(async move |view, cx| {
                        if let Err(error) = job.await {
                            let _ = view.update(cx, |view, cx| {
                                view.error = Some(format!("Conversation deleted. Worktree cleanup failed. {error:#}"));
                                cx.notify();
                            });
                        }
                    }).detach();
                }
                view.report(result, cx);
            });
        })
        .detach();
    }
}
