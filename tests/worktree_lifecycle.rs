use huggler::{
    Action, Command, CommandId, Huggler, HugglerOptions, ThreadId,
    workspace::{OrphanedWorktreeOutcome, WorktreeBranch},
};
use std::{fs, path::Path, process::Command as Process};
use uuid::Uuid;

fn git(root: &Path, args: &[&str]) -> String {
    let result = Process::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

fn dispatch(runtime: &Huggler, thread: &ThreadId, action: Action) -> huggler::Result<()> {
    runtime.dispatch(Command {
        id: CommandId(Uuid::new_v4().to_string()),
        thread_id: thread.clone(),
        action,
    })?;
    Ok(())
}

#[tokio::test]
async fn settled_checkout_is_reclaimed_and_restored_without_deleting_history() -> anyhow::Result<()>
{
    use OrphanedWorktreeOutcome::{Kept, NeedsMergeVerification, Removable, Removed};

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?.join("project");
    fs::create_dir(&root)?;
    git(&root, &["init", "-qb", "main"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.com"]);
    fs::write(root.join("tracked"), "initial")?;
    fs::write(root.join(".gitignore"), "local-cache\n")?;
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "Initial"]);
    let options = HugglerOptions::new(temp.path().join("state"));
    let runtime = Huggler::open(options.clone()).await?;
    let project = runtime.open_project(&root)?;
    let linked = runtime.allocate_workspace_on_branch(
        &project.id,
        "HEAD",
        WorktreeBranch::Exact("topic"),
    )?;
    let path = &linked.root;
    let thread = ThreadId("task".into());
    let shared = ThreadId("shared".into());
    for id in [&thread, &shared] {
        dispatch(
            &runtime,
            id,
            Action::Launch {
                project: linked.id.clone(),
                name: id.0.clone(),
                model: None,
            },
        )?;
    }
    let settle = |id| dispatch(&runtime, id, Action::SetSettled { settled: true });
    settle(&thread)?;
    assert_eq!(runtime.reclaim_managed_worktree(path, None)?.outcome, Kept);

    settle(&shared)?;
    assert_eq!(
        runtime.inspect_managed_worktree(path, None)?.outcome,
        Removable
    );
    // Rebase can pause with HEAD at the recorded base and an empty porcelain status.
    let git_dir = git(path, &["rev-parse", "--absolute-git-dir"]);
    let rebase = Path::new(&git_dir).join("rebase-merge");
    fs::create_dir(&rebase)?;
    assert_eq!(runtime.reclaim_managed_worktree(path, None)?.outcome, Kept);
    fs::remove_dir(&rebase)?;
    fs::write(path.join("tracked"), "topic work")?;
    git(path, &["commit", "-qam", "Topic"]);
    let verified = git(path, &["rev-parse", "HEAD"]);
    assert_eq!(
        runtime.reclaim_managed_worktree(path, None)?.outcome,
        NeedsMergeVerification
    );

    git(
        path,
        &["commit", "--allow-empty", "-qm", "Later local work"],
    );
    assert_eq!(
        runtime
            .reclaim_managed_worktree(path, Some(&verified))?
            .outcome,
        NeedsMergeVerification
    );
    git(path, &["reset", "--hard", &verified]);

    fs::write(path.join("local-cache"), "keep even ignored files")?;
    assert_eq!(
        runtime
            .reclaim_managed_worktree(path, Some(&verified))?
            .outcome,
        Kept
    );
    fs::remove_file(path.join("local-cache"))?;
    git(path, &["update-index", "--assume-unchanged", "tracked"]);
    assert_eq!(
        runtime
            .reclaim_managed_worktree(path, Some(&verified))?
            .outcome,
        Kept
    );
    git(path, &["update-index", "--no-assume-unchanged", "tracked"]);

    let history = runtime.detail(&thread, None, "", 32)?.history.sequence;
    assert_eq!(
        runtime
            .reclaim_managed_worktree(path, Some(&verified))?
            .outcome,
        Removed
    );
    assert!(!path.exists());
    assert_eq!(git(&root, &["rev-parse", "refs/heads/topic"]), verified);
    assert!(runtime.worktree_released(path)?);
    assert_eq!(
        runtime.detail(&thread, None, "", 32)?.history.sequence,
        history
    );
    assert!(dispatch(&runtime, &thread, Action::SetSettled { settled: false }).is_err());
    assert!(
        runtime
            .send(
                CommandId("no-checkout".into()),
                shared.clone(),
                "Don't run".into()
            )
            .is_err()
    );

    runtime.close().await?;
    let runtime = Huggler::open(options).await?;
    assert!(runtime.worktree_released(path)?);
    let source = runtime.released_worktree_source(path)?;
    assert_eq!(source, root);
    runtime.restore_managed_worktree(path, &source, "refs/heads/main")?;
    assert!(!runtime.worktree_released(path)?);
    assert_eq!(
        git(path, &["rev-parse", "HEAD"]),
        git(&root, &["rev-parse", "HEAD"])
    );
    assert_ne!(git(path, &["branch", "--show-current"]), "topic");
    dispatch(&runtime, &thread, Action::SetSettled { settled: false })?;
    assert!(!runtime.detail(&thread, None, "", 32)?.settled);
    assert_eq!(runtime.thread_checkout(&thread)?, *path);
    let git_dir = git(path, &["rev-parse", "--absolute-git-dir"]);
    let rebase = Path::new(&git_dir).join("rebase-merge");
    fs::create_dir(&rebase)?;
    fs::remove_dir_all(path)?;
    assert_eq!(runtime.released_worktree_source(path)?, root);
    assert!(
        runtime
            .restore_managed_worktree(path, &root, "refs/heads/main")
            .is_err()
    );
    assert!(rebase.is_dir());
    fs::remove_dir(&rebase)?;
    runtime.restore_managed_worktree(path, &root, "refs/heads/main")?;
    assert!(path.is_dir());

    runtime.delete_thread(&thread)?;
    assert_eq!(
        runtime.reclaim_unreferenced_worktree(path, None)?.outcome,
        Kept
    );
    runtime.delete_thread(&shared)?;
    assert_eq!(
        runtime.reclaim_unreferenced_worktree(path, None)?.outcome,
        Removed
    );
    assert!(!path.exists());
    runtime.close().await?;
    Ok(())
}
