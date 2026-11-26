use super::*;

use crate::{git, github::Checkout, open_in::OpenIn};
use huggler::workspace::{
    ManagedWorktree, OrphanedWorktreeEntry, OrphanedWorktreeOutcome, OrphanedWorktreeReport,
};

#[derive(Default)]
pub(super) struct Inventory {
    managed_root: PathBuf,
    rows: Vec<Worktree>,
    protected: HashSet<PathBuf>,
    pending: Option<Operation>,
    status: Option<String>,
    error: Option<String>,
}

impl Inventory {
    pub(super) fn removing(&self) -> bool {
        matches!(
            self.pending,
            Some(Operation::Cleanup | Operation::Remove | Operation::Orphans)
        )
    }

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
    managed: bool,
    active: usize,
    cleanup: Option<(OrphanedWorktreeOutcome, String)>,
}

#[derive(Clone, Copy)]
enum Operation {
    Refresh,
    Cleanup,
    Remove,
    Orphans,
}

fn inventory(
    runtime: &Huggler,
    prefs: &Preferences,
    managed_root: &Path,
    assessed: Vec<(ManagedWorktree, OrphanedWorktreeEntry)>,
) -> Result<Vec<Worktree>> {
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
    }

    for (worktree, entry) in assessed {
        if worktree.released && !worktree.path.exists() && !worktree.threads.is_empty() {
            continue;
        }
        let row = rows
            .entry(entry.path.clone())
            .or_insert_with(|| Worktree::at(entry.path));
        row.managed = true;
        row.active = worktree.active;
        row.cleanup = Some((entry.outcome, entry.reason));
    }
    for (path, count) in checkouts {
        if !runtime.worktree_released(&path)?
            && (prefs.worktree_projects.contains_key(&path) || rows.contains_key(&path))
        {
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
            managed: false,
            active: 0,
            cleanup: None,
        }
    }

    fn status(&self) -> String {
        match &self.cleanup {
            Some((OrphanedWorktreeOutcome::Removable, _)) => "Ready to remove".into(),
            Some((_, reason)) => reason.clone(),
            None if self.conversations == 1 => "Used by 1 conversation".into(),
            None if self.conversations > 1 => {
                format!("Used by {} conversations", self.conversations)
            }
            None => "Git worktree · managed outside Bear".into(),
        }
    }
}

fn kept(path: &Path, reason: &str) -> OrphanedWorktreeEntry {
    OrphanedWorktreeEntry {
        path: path.to_owned(),
        outcome: OrphanedWorktreeOutcome::Kept,
        reason: reason.into(),
        bytes: None,
    }
}

struct Assessment {
    worktree: ManagedWorktree,
    entry: OrphanedWorktreeEntry,
    merged_head: Option<String>,
}

async fn assess(
    runtime: Arc<Huggler>,
    prefs: &Preferences,
    protected: &HashSet<PathBuf>,
    only_unreferenced: Option<&Path>,
) -> Result<Vec<Assessment>> {
    let worktrees = runtime.managed_worktrees()?;
    let mut results = Vec::new();
    let mut checks = tokio::task::JoinSet::new();
    let github_slots = Arc::new(tokio::sync::Semaphore::new(4));
    for worktree in worktrees {
        let path = &worktree.path;
        if only_unreferenced.is_some_and(|target| target != path || !worktree.threads.is_empty()) {
            continue;
        }
        let entry = if protected.contains(path) {
            kept(path, "In use by an open terminal or editor")
        } else if prefs.projects.iter().any(|project| &project.root == path) {
            kept(path, "Saved as a project")
        } else {
            runtime.inspect_managed_worktree(path, None)?
        };
        if entry.outcome == OrphanedWorktreeOutcome::NeedsMergeVerification {
            let thread = worktree.threads.first().cloned();
            let repository_root = if thread.is_none() {
                let Ok(main) = git::main_worktree(path) else {
                    results.push(Assessment {
                        worktree,
                        entry,
                        merged_head: None,
                    });
                    continue;
                };
                Some(main)
            } else {
                None
            };
            let checkout = Checkout {
                runtime: runtime.clone(),
                thread,
                root: path.clone(),
                repository_root,
            };
            let github_slots = github_slots.clone();
            checks.spawn(async move {
                let _slot = github_slots.acquire().await.expect("semaphore stays open");
                let merged_head = tokio::time::timeout(
                    std::time::Duration::from_secs(8),
                    checkout.current_merged_head(),
                )
                .await
                .ok()
                .flatten();
                Assessment {
                    worktree,
                    entry,
                    merged_head,
                }
            });
        } else {
            results.push(Assessment {
                worktree,
                entry,
                merged_head: None,
            });
        }
    }
    while let Some(result) = checks.join_next().await {
        let mut assessment = result?;
        if let Some(head) = &assessment.merged_head {
            assessment.entry =
                runtime.inspect_managed_worktree(&assessment.worktree.path, Some(head))?;
        }
        results.push(assessment);
    }
    Ok(results)
}

pub(crate) async fn reclaim_deleted_checkout(
    runtime: Arc<Huggler>,
    prefs: &Preferences,
    protected: &HashSet<PathBuf>,
    path: &Path,
) -> Result<()> {
    if let Some(assessment) = assess(runtime.clone(), prefs, protected, Some(path))
        .await?
        .pop()
        && assessment.entry.outcome == OrphanedWorktreeOutcome::Removable
    {
        runtime.reclaim_unreferenced_worktree(path, assessment.merged_head.as_deref())?;
    }
    Ok(())
}

impl Settings {
    pub(crate) fn protect_open_worktrees(&mut self, paths: HashSet<PathBuf>) {
        self.worktrees.protected = paths;
    }

    pub(super) fn refresh_worktrees(&mut self, cx: &mut Context<Self>) {
        self.run_worktree_operation(Operation::Refresh, None, cx);
    }

    fn run_worktree_operation(
        &mut self,
        operation: Operation,
        target: Option<PathBuf>,
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
        let executor = self.executor.clone();
        let managed_root = self.worktrees.managed_root.clone();
        let protected = self.worktrees.protected.clone();
        let job = cx.background_spawn(async move {
            let automatic_result = matches!(operation, Operation::Orphans).then(|| {
                let mut exclusions = prefs.saved_project_roots();
                exclusions.extend(protected.iter().cloned());
                runtime.cleanup_orphaned_worktrees_excluding(&exclusions)
            });
            let assessed = executor.block_on(assess(runtime.clone(), &prefs, &protected, None));
            let assessed = match assessed {
                Ok(assessed) => assessed,
                Err(error) => return (Err(error), Ok(Vec::new())),
            };
            let mut status = match automatic_result {
                Some(result) => result
                    .map(|report| Some(cleanup_summary(&report)))
                    .map_err(Into::into),
                None => Ok(None),
            };
            let mut report = OrphanedWorktreeReport::default();
            let mut remaining = Vec::new();
            for Assessment {
                worktree,
                entry,
                merged_head,
            } in assessed
            {
                let selected = target.as_ref().is_some_and(|path| *path == worktree.path);
                let idle = worktree.active == 0
                    && !worktree.busy
                    && (!worktree.released || worktree.threads.is_empty());
                if selected || (matches!(operation, Operation::Cleanup) && idle) {
                    let entry = if entry.outcome == OrphanedWorktreeOutcome::Removable {
                        match runtime
                            .reclaim_managed_worktree(&worktree.path, merged_head.as_deref())
                        {
                            Ok(entry) => entry,
                            Err(error) => OrphanedWorktreeEntry {
                                outcome: OrphanedWorktreeOutcome::Failed,
                                reason: error.to_string(),
                                ..entry
                            },
                        }
                    } else {
                        entry
                    };
                    report.entries.push(entry.clone());
                    if selected {
                        status = if entry.outcome == OrphanedWorktreeOutcome::Removed {
                            Ok(Some("Worktree removed.".into()))
                        } else {
                            Err(anyhow::anyhow!(
                                "Could not remove {}. {}",
                                worktree.path.display(),
                                entry.reason
                            ))
                        };
                    }
                    if entry.outcome != OrphanedWorktreeOutcome::Removed {
                        remaining.push((worktree, entry));
                    }
                } else {
                    remaining.push((worktree, entry));
                }
            }
            if matches!(operation, Operation::Cleanup) {
                status = Ok(Some(cleanup_summary(&report)));
            } else if matches!(operation, Operation::Remove) && report.entries.is_empty() {
                status = Err(anyhow::anyhow!("This worktree is no longer available."));
            }
            (
                status,
                inventory(&runtime, &prefs, &managed_root, remaining),
            )
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

    fn remove_worktree(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(all(feature = "qualification", target_os = "macos"))]
        let scripted = crate::qualification::take_prompt_answer(cx);
        #[cfg(not(all(feature = "qualification", target_os = "macos")))]
        let scripted: Option<usize> = None;
        let prompt = scripted.is_none().then(|| {
            window.prompt(
                PromptLevel::Warning,
                "Remove this worktree?",
                Some(&format!(
                    "Bear will check {} again before removing it. Saved conversations will remain in history.",
                    path.display()
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
                    view.run_worktree_operation(Operation::Remove, Some(path), cx);
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
        let remove_path = path.clone();
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
                    .when(row.managed && row.active == 0, |actions| {
                        actions.child(control!(
                            format!("Remove worktree/{location}"),
                            Button::new("remove-worktree")
                                .small()
                                .ghost()
                                .label("Remove")
                                .disabled(self.worktrees.pending.is_some())
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.remove_worktree(remove_path.clone(), window, cx);
                                }))
                        ))
                    }),
            )
            .child(hint(location, cx).text_ellipsis())
            .child(control!(row.status(), hint(row.status(), cx)))
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
                                        view.run_worktree_operation(Operation::Orphans, None, cx);
                                    }
                                }))
                        )
                    })),
            )
            .child(hint(
                "When on, Bear checks at startup and hourly. The background check removes abandoned, unchanged checkouts. Use Clean up now to also reclaim worktrees from settled conversations.",
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
        self.run_worktree_operation(Operation::Cleanup, None, cx);
    }
}

fn cleanup_summary(report: &OrphanedWorktreeReport) -> String {
    if report.entries.is_empty() {
        return "No idle managed worktrees to clean up.".into();
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
    let kept = count(OrphanedWorktreeOutcome::Kept)
        + count(OrphanedWorktreeOutcome::NeedsMergeVerification);
    if kept > 0 {
        parts.push(format!("kept {kept} for review"));
    }
    let failed = count(OrphanedWorktreeOutcome::Failed);
    if failed > 0 {
        parts.push(format!("retry later {failed}"));
    }
    format!("{}.", parts.join(", "))
}
