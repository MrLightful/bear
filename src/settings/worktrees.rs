use super::*;

use crate::{git, open_in::OpenIn};
use huggler::{
    Project,
    workspace::{OrphanedWorktreeOutcome, OrphanedWorktreeReport},
};

#[derive(Default)]
pub(super) struct Inventory {
    managed_root: PathBuf,
    rows: Vec<Worktree>,
    pending: Option<Operation>,
    status: Option<String>,
    error: Option<String>,
}

impl Inventory {
    pub(super) fn new(managed_root: PathBuf) -> Self {
        Self {
            managed_root,
            ..Self::default()
        }
    }
}

struct Worktree {
    path: PathBuf,
    branch: Option<String>,
    conversations: usize,
    allocation: Option<Project>,
    cleanup: Option<(OrphanedWorktreeOutcome, String)>,
}

#[derive(Clone, Copy)]
enum Operation {
    Refresh,
    Cleanup,
    Remove,
}

fn inventory(runtime: &Huggler, prefs: &Preferences, managed_root: &Path) -> Result<Vec<Worktree>> {
    let mut checkouts = BTreeMap::new();
    for thread in runtime.threads()? {
        match runtime.thread_checkout(&thread.id) {
            Ok(path) => *checkouts.entry(path).or_insert(0) += 1,
            // The conversation may have been deleted since the list was read.
            Err(huggler::Error::Forbidden) => continue,
            Err(error) => return Err(error.into()),
        }
    }

    let mut rows = BTreeMap::new();
    let mut repositories = HashSet::new();
    let roots = prefs
        .projects
        .iter()
        .map(|project| &project.root)
        .chain(prefs.worktree_projects.values())
        .chain(checkouts.keys());
    let mut managed = Vec::new();
    match fs::read_dir(managed_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir() && entry.path().join(".git").is_file() {
                    managed.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    for root in roots
        .chain(&managed)
        .collect::<std::collections::BTreeSet<_>>()
    {
        if rows.contains_key(root) || repositories.contains(root) {
            continue;
        }
        let Ok((main, linked)) = git::linked_worktrees(root) else {
            // Scratch directories and checkouts on disconnected volumes have no local Git list.
            continue;
        };
        if !repositories.insert(main.clone()) {
            continue;
        }
        for checkout in linked {
            rows.insert(
                checkout.path.clone(),
                Worktree {
                    branch: checkout.branch,
                    ..Worktree::at(checkout.path)
                },
            );
        }
        // Registering a linked checkout as a project would prevent orphan cleanup.
        if let Ok(project) = runtime.open_project(&main)
            && project.root == main
        {
            for allocation in runtime.allocated_workspaces(&project.id)? {
                let path = allocation.root.clone();
                let row = rows
                    .entry(path.clone())
                    .or_insert_with(|| Worktree::at(path));
                row.allocation = Some(allocation);
            }
        }
    }

    for entry in runtime.inspect_orphaned_worktrees()?.entries {
        rows.entry(entry.path.clone())
            .or_insert_with(|| Worktree::at(entry.path.clone()))
            .cleanup = Some((entry.outcome, entry.reason));
    }
    for (path, count) in checkouts {
        if prefs.worktree_projects.contains_key(&path) || rows.contains_key(&path) {
            rows.entry(path.clone())
                .or_insert_with(|| Worktree::at(path))
                .conversations = count;
        }
    }
    Ok(rows.into_values().collect())
}

impl Worktree {
    fn at(path: PathBuf) -> Self {
        Self {
            path,
            branch: None,
            conversations: 0,
            allocation: None,
            cleanup: None,
        }
    }

    fn status(&self) -> String {
        match self.conversations {
            1 => "Used by 1 conversation".into(),
            count if count > 1 => format!("Used by {count} conversations"),
            _ => match &self.cleanup {
                Some((OrphanedWorktreeOutcome::Removable, _)) => "Ready for cleanup".into(),
                Some((_, reason)) => reason.clone(),
                None if self.allocation.is_some() => "Unused managed worktree".into(),
                None => "Git worktree".into(),
            },
        }
    }
}

fn remove_allocation(runtime: &Huggler, project: &Project) -> Result<()> {
    git::ensure_worktree_removal_safe(&project.root)?;
    runtime.release_workspace(&project.id)?;
    Ok(())
}

impl Settings {
    pub(super) fn refresh_worktrees(&mut self, cx: &mut Context<Self>) {
        self.run_worktree_operation(Operation::Refresh, |_| Ok(None), cx);
    }

    fn run_worktree_operation(
        &mut self,
        operation: Operation,
        action: impl FnOnce(&Huggler) -> Result<Option<String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.worktrees.pending.is_some() {
            return;
        }
        self.worktrees.pending = Some(operation);
        self.worktrees.status = None;
        self.worktrees.error = None;
        let runtime = self.runtime.clone();
        let prefs = self.prefs.clone();
        let managed_root = self.worktrees.managed_root.clone();
        let job = cx.background_spawn(async move {
            let status = action(&runtime);
            (status, inventory(&runtime, &prefs, &managed_root))
        });
        cx.spawn(async move |view, cx| {
            let (status, rows) = job.await;
            let _ = view.update(cx, |view, cx| {
                view.worktrees.pending = None;
                match status {
                    Ok(status) => view.worktrees.status = status,
                    Err(error) => view.worktrees.error = Some(format!("{error:#}")),
                }
                match rows {
                    Ok(rows) => view.worktrees.rows = rows,
                    Err(error) => {
                        view.worktrees.rows.clear();
                        let message = format!("Could not list worktrees. {error}");
                        match &mut view.worktrees.error {
                            Some(error) => {
                                error.push(' ');
                                error.push_str(&message);
                            }
                            None => view.worktrees.error = Some(message),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn remove_worktree(&mut self, project: Project, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(all(feature = "qualification", target_os = "macos"))]
        let scripted = crate::qualification::take_prompt_answer(cx);
        #[cfg(not(all(feature = "qualification", target_os = "macos")))]
        let scripted: Option<usize> = None;
        let prompt = scripted.is_none().then(|| {
            window.prompt(
                PromptLevel::Warning,
                "Remove this worktree?",
                Some(&format!(
                    "Bear will remove {} if it is unused and unchanged.",
                    project.root.display()
                )),
                &["Cancel", "Remove"],
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
                    view.run_worktree_operation(
                        Operation::Remove,
                        move |runtime| {
                            remove_allocation(runtime, &project).with_context(|| {
                                format!("Could not remove {}.", project.root.display())
                            })?;
                            Ok(Some("Worktree removed.".into()))
                        },
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    pub(super) fn worktrees_panel(&self, cx: &Context<Self>) -> Div {
        let busy = self.worktrees.pending.is_some();
        let rows = &self.worktrees.rows;
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(section_title("Local worktrees").flex_1())
                            .child(self.cleanup_button(cx))
                            .child(control!(
                                "Refresh worktrees",
                                Button::new("refresh-worktrees")
                                    .small()
                                    .ghost()
                                    .label(match self.worktrees.pending {
                                        Some(Operation::Refresh) => "Refreshing…",
                                        Some(Operation::Remove) => "Removing…",
                                        _ => "Refresh",
                                    })
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.refresh_worktrees(cx))
                                    )
                            )),
                    )
                    .when_some(self.worktrees.status.clone(), |list, status| {
                        list.child(control!(status.clone(), hint(status, cx)))
                    })
                    .when(rows.is_empty() && self.worktrees.error.is_none(), |list| {
                        list.child(hint(
                            if busy {
                                "Loading worktrees…"
                            } else {
                                "No local worktrees found."
                            },
                            cx,
                        ))
                    })
                    .when(!rows.is_empty(), |list| {
                        list.child(
                            v_flex()
                                .rounded_lg()
                                .overflow_hidden()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().sidebar)
                                .children(
                                    rows.iter()
                                        .enumerate()
                                        .map(|(index, row)| self.worktree_row(index, row, cx)),
                                ),
                        )
                    })
                    .when_some(self.worktrees.error.clone(), |list, error| {
                        list.child(control!(
                            error.clone(),
                            div().text_sm().text_color(cx.theme().danger).child(error)
                        ))
                    }),
            )
            .child(self.worktree_cleanup(cx))
    }

    fn worktree_row(&self, index: usize, row: &Worktree, cx: &Context<Self>) -> impl IntoElement {
        let path = row.path.clone();
        let location = path.display().to_string();
        let title = row.branch.clone().unwrap_or_else(|| {
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned()
        });
        v_flex()
            .id(("worktree", index))
            .gap_1()
            .px_3()
            .py_2()
            .when(index > 0, |row| {
                row.border_t_1().border_color(cx.theme().border)
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(label(title).flex_1().min_w_0().truncate())
                    .child(control!(
                        format!("Open in Finder/{location}"),
                        Button::new("open-worktree")
                            .small()
                            .ghost()
                            .label("Open in Finder")
                            .tooltip(location.clone())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.worktrees.error = OpenIn::Finder
                                    .open_workspace(&path)
                                    .err()
                                    .map(|error| format!("{error:#}"));
                                cx.notify();
                            }))
                    ))
                    .when_some(
                        row.allocation.clone().filter(|_| row.conversations == 0),
                        |actions, project| {
                            actions.child(control!(
                                format!("Remove worktree/{location}"),
                                Button::new("remove-worktree")
                                    .small()
                                    .ghost()
                                    .label("Remove")
                                    .disabled(self.worktrees.pending.is_some())
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.remove_worktree(project.clone(), window, cx);
                                    }))
                            ))
                        },
                    ),
            )
            .child(hint(location, cx).text_ellipsis())
            .child(hint(row.status(), cx))
    }

    fn worktree_cleanup(&self, cx: &Context<Self>) -> Div {
        v_flex()
            .gap_2()
            .child(section_title("Automatic cleanup"))
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children([(false, "Off"), (true, "On")].map(|(enabled, name)| {
                        control!(
                            format!("automatic-cleanup/{name}"),
                            choice(name, name, self.prefs.automatic_worktree_cleanup == enabled)
                                .disabled(self.worktrees.pending.is_some())
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    if view.persist(
                                        Preferences {
                                            automatic_worktree_cleanup: enabled,
                                            ..view.prefs.clone()
                                        },
                                        cx,
                                    ) && enabled
                                    {
                                        view.cleanup_worktrees(cx);
                                    }
                                }))
                        )
                    })),
            )
            .child(hint(
                "When on, Bear checks at startup and hourly. Cleanup removes eligible worktrees with no local changes or new commits.",
                cx,
            ))
    }

    fn cleanup_button(&self, cx: &Context<Self>) -> Div {
        control!(
            "Clean up worktrees now",
            Button::new("cleanup-worktrees")
                .small()
                .label(
                    if matches!(self.worktrees.pending, Some(Operation::Cleanup)) {
                        "Cleaning…"
                    } else {
                        "Clean up now"
                    }
                )
                .disabled(self.worktrees.pending.is_some())
                .on_click(cx.listener(|view, _, _, cx| view.cleanup_worktrees(cx)))
        )
    }

    fn cleanup_worktrees(&mut self, cx: &mut Context<Self>) {
        self.run_worktree_operation(
            Operation::Cleanup,
            |runtime| {
                let report = runtime
                    .cleanup_orphaned_worktrees()
                    .context("Could not clean up worktrees")?;
                Ok(Some(cleanup_summary(&report)))
            },
            cx,
        );
    }
}

fn cleanup_summary(report: &OrphanedWorktreeReport) -> String {
    if report.entries.is_empty() {
        return "No unused worktrees found.".into();
    }
    let count = |outcome| {
        report
            .entries
            .iter()
            .filter(|entry| entry.outcome == outcome)
            .count()
    };
    let mut parts = vec![format!(
        "Removed {}",
        count(OrphanedWorktreeOutcome::Removed)
    )];
    let kept = count(OrphanedWorktreeOutcome::Kept);
    if kept > 0 {
        parts.push(format!("kept {kept} for review"));
    }
    let failed = count(OrphanedWorktreeOutcome::Failed);
    if failed > 0 {
        parts.push(format!("retry later {failed}"));
    }
    format!("{}.", parts.join(", "))
}
