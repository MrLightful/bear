//! Connect the selected checkout to Huggler's repository client using GitHub CLI credentials.
use crate::git;
use anyhow::{Context, Result, bail, ensure};
use huggler::{
    Huggler, ThreadId,
    repository::{
        Driver, RepositoryClient,
        github::{
            BranchPullRequests, MergeMethod, PullRequest, PullRequestState, PullRequestUpdate,
        },
    },
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Create,
    Resolve,
    Fix,
    Update(PullRequestUpdate),
    View,
    Draft,
    AutoMergeEnabled,
    Queued,
    Merged,
    Closed,
}

#[derive(Clone, Debug)]
pub enum Status {
    Hidden,
    Create {
        branch: git::GitHubBranch,
        unavailable: Option<String>,
    },
    Conflicts,
    Found {
        branch: git::GitHubBranch,
        head: String,
        pr: Box<PullRequest>,
    },
    Multiple(Vec<PullRequest>),
}

impl Status {
    pub fn action(&self) -> Option<Action> {
        Some(match self {
            Self::Hidden => return None,
            Self::Create { .. } => Action::Create,
            Self::Conflicts => Action::Resolve,
            Self::Found { branch, pr, .. } => primary_action(branch, pr),
            Self::Multiple(_) => Action::View,
        })
    }
}

fn primary_action(branch: &git::GitHubBranch, pr: &PullRequest) -> Action {
    match pr.state() {
        PullRequestState::Merged => return Action::Merged,
        PullRequestState::Closed => return Action::Closed,
        PullRequestState::Unknown => return Action::View,
        PullRequestState::Open => {}
    }
    if pr.has_conflicts() {
        return Action::Resolve;
    }
    if pr.failed_checks() || pr.changes_requested() {
        return Action::Fix;
    }
    if pr.is_draft() {
        return Action::Draft;
    }
    if pr.is_queued() {
        return Action::Queued;
    }
    if pr.auto_merge_enabled() {
        return Action::AutoMergeEnabled;
    }
    if pr.head_commit() != branch.local_oid {
        return Action::View;
    }
    let method = [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase]
        .into_iter()
        .find(|method| pr.allows_merge_method(*method));
    let update = match (pr.uses_merge_queue(), method) {
        (true, _) if pr.can_merge_now() => PullRequestUpdate::Enqueue,
        (false, Some(method)) if pr.can_merge_now() => PullRequestUpdate::Merge(method),
        (true, _) if pr.can_enable_auto_merge() => PullRequestUpdate::EnableAutoMerge(None),
        (false, Some(method)) if pr.can_enable_auto_merge() => {
            PullRequestUpdate::EnableAutoMerge(Some(method))
        }
        _ => return Action::View,
    };
    Action::Update(update)
}

#[derive(Clone)]
pub(super) struct Checkout {
    pub runtime: Arc<Huggler>,
    pub thread: Option<ThreadId>,
    pub root: PathBuf,
    pub repository_root: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct PublishTarget {
    root: PathBuf,
    branch: git::GitHubBranch,
    configured_base: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequestText {
    head_oid: String,
    pending_tree: Option<String>,
    title: String,
    body: String,
    commit_subject: Option<String>,
}

pub fn create_request(root: &Path, branch: &git::GitHubBranch) -> Result<huggler::Action> {
    let target = PublishTarget {
        root: root.to_owned(),
        branch: branch.clone(),
        configured_base: branch.configured_pr_base(root)?,
    };
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .context("The Bear executable path is not UTF-8.")?;
    let target_json = serde_json::to_string(&target)?;
    let command = |mode| -> Result<String> {
        Ok([executable, mode, &target_json]
            .map(shlex::try_quote)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .join(" "))
    };
    let context = format!(
        "Create a pull request from this conversation’s checkout. Run Inspect first and follow the bundled Bear skill returned in the `skill` field.\n\nInspect:\n\n```sh\n{}\n```\n\nPublish (send the PR payload on stdin):\n\n```sh\n{}\n```",
        command("--inspect-pull-request")?,
        command("--publish-pull-request")?,
    );
    Ok(huggler::Action::EnqueueFiles {
        text: "/create-pr".into(),
        files: vec![huggler::Attachment {
            name: "PR context.txt".into(),
            mime: "text/plain".into(),
            bytes: context.into_bytes(),
        }],
    })
}

impl PublishTarget {
    fn validate(&self) -> Result<git::GitHubBranch> {
        let branch = git::github_branch(&self.root)?;
        ensure!(
            branch.same_branch(&self.branch)
                && branch.configured_pr_base(&self.root)? == self.configured_base,
            "The checkout or PR base changed since Create PR was queued. Try again."
        );
        Ok(branch)
    }
}

pub fn inspect_pull_request(target_json: &str) -> Result<serde_json::Value> {
    let target: PublishTarget = serde_json::from_str(target_json)?;
    let branch = target.validate()?;
    let pending = git::PendingCommit::prepare(&target.root, &branch)?;
    Ok(serde_json::json!({
        "repository": format!("{}/{}", branch.owner, branch.repo),
        "configured_base": target.configured_base,
        "head_oid": branch.local_oid,
        "pending_tree": pending.as_ref().map(git::PendingCommit::tree_oid),
        "skill": include_str!("../assets/skills/create-pr/SKILL.md"),
    }))
}

pub async fn publish_from_stdin(target_json: &str) -> Result<String> {
    let target: PublishTarget = serde_json::from_str(target_json)?;
    let mut input = Vec::new();
    const MAX_PAYLOAD: u64 = 128 * 1024;
    io::stdin()
        .lock()
        .take(MAX_PAYLOAD + 1)
        .read_to_end(&mut input)?;
    ensure!(
        input.len() as u64 <= MAX_PAYLOAD,
        "The PR payload is too large."
    );
    let text: PullRequestText =
        serde_json::from_slice(&input).context("Read PR title and description")?;
    ensure!(
        valid_subject(&text.title),
        "The PR title must be a single nonempty line."
    );
    ensure!(!text.body.trim().is_empty(), "The PR description is empty.");
    if let Some(subject) = &text.commit_subject {
        ensure!(
            valid_subject(subject),
            "The commit subject must be a single nonempty line."
        );
    }
    let state = tempfile::tempdir()?;
    let runtime = Arc::new(Huggler::open(huggler::HugglerOptions::new(state.path())).await?);
    let checkout = Checkout {
        runtime,
        thread: None,
        root: target.root.clone(),
        repository_root: None,
    };
    let result = checkout.create_pull_request(&target, &text).await;
    checkout.runtime.close().await?;
    result
}

fn valid_subject(subject: &str) -> bool {
    !subject.trim().is_empty() && !subject.chars().any(char::is_control)
}

struct PullRequestTarget {
    branch: git::GitHubBranch,
    default: String,
    configured_base: Option<String>,
}

impl PullRequestTarget {
    fn resolve(branch: &git::GitHubBranch, root: &Path, default: String) -> Result<Self> {
        let configured_base = branch.configured_pr_base(root)?;
        let base = configured_base.as_deref().unwrap_or(&default);
        Ok(Self {
            branch: branch.clone().pr_head(&default, base),
            default,
            configured_base,
        })
    }

    fn base(&self) -> &str {
        self.configured_base.as_deref().unwrap_or(&self.default)
    }

    fn base_commit(&self, root: &Path) -> Option<String> {
        self.branch.pr_base_commit(root, self.base())
    }

    fn can_create(&self, root: &Path, dirty: bool) -> bool {
        self.branch.is_pr_head(&self.default, self.base())
            && (dirty
                || self
                    .base_commit(root)
                    .is_some_and(|base| self.branch.has_pr_commits(root, &base)))
    }

    fn compare_url(&self, root: &Path) -> Result<String> {
        ensure!(
            !git::has_uncommitted_changes(root)?,
            "GitHub status is required to include uncommitted changes in a pull request."
        );
        ensure!(
            self.can_create(root, false),
            "Create PR needs commits on a topic branch beyond {}.",
            self.base()
        );
        self.branch.compare_url(&self.default, self.base())
    }
}

impl Checkout {
    /// Recheck the local checkout after the network lookup before applying policy.
    pub async fn has_current_merged_pr(&self) -> bool {
        self.current_merged_head().await.is_some()
    }

    pub async fn current_merged_head(&self) -> Option<String> {
        let Status::Found { branch, pr, .. } = self.status().await else {
            return None;
        };
        if pr.state() == PullRequestState::Merged
            && pr.head_commit() == branch.local_oid
            && git::has_uncommitted_changes(&self.root).is_ok_and(|dirty| !dirty)
            && git::github_branch(&self.root).is_ok_and(|current| current.same_target(&branch))
        {
            return Some(branch.local_oid);
        }
        None
    }

    pub async fn status(&self) -> Status {
        if git::has_conflicts(&self.root) {
            return Status::Conflicts;
        }
        let Ok(branch) = git::github_branch(&self.root) else {
            return Status::Hidden;
        };
        self.branch_status(branch).await
    }

    async fn branch_status(&self, branch: git::GitHubBranch) -> Status {
        let client = match self.client(&branch).await {
            Ok(client) => client,
            Err(error) => return self.offline_create_status(&branch, error),
        };
        let (target, base_error) = match client.github_default_branch().await {
            Ok(default) => (
                PullRequestTarget::resolve(&branch, &self.root, default).ok(),
                None,
            ),
            Err(error) => (self.cached_target(&branch).ok(), Some(error.to_string())),
        };
        let head = &target
            .as_ref()
            .map_or(&branch, |target| &target.branch)
            .head;
        let dirty = git::has_uncommitted_changes(&self.root).unwrap_or(false);
        let error = match client.github_pull_requests(head).await {
            Ok(requests) => {
                let status = select_pull_request(branch.clone(), head, requests, dirty);
                if !matches!(status, Status::Create { .. }) {
                    return status;
                }
                base_error
            }
            Err(error) => Some(error.to_string()),
        };
        if target.is_some_and(|target| target.can_create(&self.root, dirty)) {
            Status::Create {
                branch,
                unavailable: error,
            }
        } else {
            Status::Hidden
        }
    }

    fn cached_target(&self, branch: &git::GitHubBranch) -> Result<PullRequestTarget> {
        let default = branch
            .cached_default_branch(&self.root)
            .context("The remote default branch is unavailable.")?;
        PullRequestTarget::resolve(branch, &self.root, default)
    }

    fn offline_create_status(&self, branch: &git::GitHubBranch, error: anyhow::Error) -> Status {
        let dirty = git::has_uncommitted_changes(&self.root).unwrap_or(false);
        if self
            .cached_target(branch)
            .is_ok_and(|target| target.can_create(&self.root, dirty))
        {
            Status::Create {
                branch: branch.clone(),
                unavailable: Some(format!("{error:#}")),
            }
        } else {
            Status::Hidden
        }
    }

    fn offline_compare_url(
        &self,
        branch: &git::GitHubBranch,
        error: anyhow::Error,
    ) -> Result<String> {
        self.verify_checkout(branch)?;
        self.cached_target(branch)
            .and_then(|target| target.compare_url(&self.root))
            .with_context(|| format!("{error:#}"))
    }

    async fn create_pull_request(
        &self,
        requested: &PublishTarget,
        text: &PullRequestText,
    ) -> Result<String> {
        let original = requested.validate()?;
        ensure!(
            original.local_oid == text.head_oid,
            "HEAD changed while drafting the pull request. Try again."
        );
        let client = match self.client(&original).await {
            Ok(client) => client,
            Err(error) => return self.offline_compare_url(&original, error),
        };
        let default = match client.github_default_branch().await {
            Ok(default) => default,
            Err(error) => return self.offline_compare_url(&original, error.into()),
        };
        let target = PullRequestTarget::resolve(&original, &self.root, default)?;
        let mut branch = target.branch.clone();
        let base = target.base();
        let requests = client.github_pull_requests(&branch.head).await;
        self.verify_checkout(&original)?;
        ensure!(
            original.configured_pr_base(&self.root)? == requested.configured_base
                && target.configured_base == requested.configured_base,
            "The pull request base changed. Try again."
        );
        let requests = match requests {
            Ok(requests) => requests,
            Err(error) => {
                return target
                    .compare_url(&self.root)
                    .with_context(|| error.to_string());
            }
        };
        let dirty = git::has_uncommitted_changes(&self.root)?;
        verify_can_create(&original, &branch.head, requests, dirty)?;
        ensure!(
            branch.is_pr_head(&target.default, base),
            "Create PR requires a topic branch."
        );
        branch.verify_push_target(&self.root)?;
        let mut expected = original;
        let pending = if dirty {
            git::PendingCommit::prepare(&self.root, &branch)?
        } else {
            None
        };
        ensure!(
            pending
                .as_ref()
                .map(git::PendingCommit::tree_oid)
                .as_deref()
                == text.pending_tree.as_deref(),
            "The working changes changed while drafting the pull request. Review them and try again."
        );
        if let Some(pending) = pending {
            let subject = text
                .commit_subject
                .as_deref()
                .context("Supply a commit subject for the pending changes.")?;
            let requests = client.github_pull_requests(&branch.head).await?;
            verify_can_create(&expected, &branch.head, requests, true)?;
            self.verify_checkout(&expected)?;
            ensure!(
                expected.configured_pr_base(&self.root)? == target.configured_base,
                "The pull request base changed. Try again."
            );
            branch.verify_push_target(&self.root)?;
            let oid = pending.commit(&self.root, subject)?;
            expected.local_oid.clone_from(&oid);
            branch.local_oid = oid;
        } else {
            ensure!(
                target
                    .base_commit(&self.root)
                    .is_some_and(|base| branch.has_pr_commits(&self.root, &base)),
                "Create PR needs changes on a topic branch beyond {base}."
            );
        }
        ensure!(
            !git::has_uncommitted_changes(&self.root)?,
            "The checkout has uncommitted changes. Review them and try again."
        );
        self.verify_checkout(&expected)?;
        branch.push(&self.root).await?;
        let current = git::github_branch(&self.root)?;
        ensure!(
            !git::has_conflicts(&self.root)
                && !git::has_uncommitted_changes(&self.root)?
                && expected.same_target(&current)
                && current.configured_pr_base(&self.root)? == target.configured_base,
            "The checkout or pull request status changed. Review the current action and try again."
        );
        branch.set_upstream(&self.root)?;

        let url = |number| {
            format!(
                "https://github.com/{}/{}/pull/{number}",
                branch.owner, branch.repo
            )
        };
        match client
            .create_github_pull_request(&branch.head, base, text.title.trim(), &text.body)
            .await
        {
            Ok(number) => Ok(url(number)),
            Err(error) => {
                // A concurrent click or a lost response can leave an open PR after a failed request.
                if let Ok(requests) = client.github_pull_requests(&branch.head).await
                    && let Some(pr) = requests.open.iter().find(|pr| {
                        pr.base_branch() == base
                            && pr.head_commit() == branch.local_oid
                            && pr
                                .repository()
                                .eq_ignore_ascii_case(&format!("{}/{}", branch.owner, branch.repo))
                    })
                {
                    return Ok(url(pr.number()));
                }
                Err(error).context(
                    "The branch was pushed. Could not confirm that GitHub created the pull request",
                )
            }
        }
    }

    pub async fn update(
        &self,
        branch: &git::GitHubBranch,
        pr: &PullRequest,
        action: PullRequestUpdate,
    ) -> Result<()> {
        let client = self.client(branch).await?;
        self.verify_checkout(branch)?;
        client.update_github_pull_request(pr, action).await?;
        Ok(())
    }

    pub async fn handoff(&self, previous: &Status, action: Action) -> Result<String> {
        match previous {
            Status::Conflicts if action == Action::Resolve && git::has_conflicts(&self.root) => Ok(
                "Resolve the Git conflicts in this checkout and continue the operation in progress."
                    .into(),
            ),
            Status::Found { branch, head, pr } => {
                let fresh = self.status().await;
                if let Status::Create { unavailable: Some(error), .. } = &fresh {
                    bail!("Could not verify the pull request. {error}");
                }
                ensure!(
                    matches!(fresh, Status::Found { branch: current, head: ref current_head, pr: updated }
                        if &current == branch
                            && current_head == head
                            && pr.same_revision(&updated)
                            && primary_action(&current, &updated) == action),
                    "The checkout or pull request status changed. Review the current action and try again."
                );
                self.verify_checkout(branch)?;
                let url = pr.url();
                match action {
                    Action::Resolve => Ok(format!(
                        "Resolve the merge conflicts for {url}, run the relevant checks, and push to its head branch."
                    )),
                    Action::Fix => Ok(format!(
                        "Fix the failed checks or address requested changes for {url}, verify the fix, and push to its head branch."
                    )),
                    _ => bail!("Unsupported pull request handoff."),
                }
            }
            _ => bail!("The checkout or pull request status changed. Review the current action and try again."),
        }
    }

    fn verify_checkout(&self, branch: &git::GitHubBranch) -> Result<()> {
        ensure!(
            !git::has_conflicts(&self.root) && git::github_branch(&self.root)?.same_target(branch),
            "The checkout or pull request status changed. Review the current action and try again."
        );
        Ok(())
    }

    async fn client(&self, branch: &git::GitHubBranch) -> Result<RepositoryClient> {
        let authorization = github_authorization().await?;
        let origin = github_api_origin();
        let repository = format!("{}/{}", branch.owner, branch.repo);
        let binding = if let Some(thread) = &self.thread {
            self.runtime
                .bind_thread_repository(thread, Driver::GitHub, &origin, &repository)?
        } else {
            let root = self.repository_root.as_deref().unwrap_or(&self.root);
            let project = self.runtime.open_project(root)?;
            self.runtime
                .bind_repository(&project.id, Driver::GitHub, &origin, &repository)?
        };
        Ok(self
            .runtime
            .repository_client(&binding.project, &binding.id, Some(&authorization))?)
    }
}

fn verify_can_create(
    branch: &git::GitHubBranch,
    head: &str,
    requests: BranchPullRequests,
    dirty: bool,
) -> Result<()> {
    ensure!(
        matches!(
            select_pull_request(branch.clone(), head, requests, dirty),
            Status::Create { .. }
        ),
        "The checkout or pull request status changed. Review the current action and try again."
    );
    Ok(())
}

fn select_pull_request(
    branch: git::GitHubBranch,
    head: &str,
    mut requests: BranchPullRequests,
    dirty: bool,
) -> Status {
    let pr = match requests.open.len() {
        0 if !dirty => {
            // A branch name can be reused for another PR after closing or merging.
            requests
                .recent
                .into_iter()
                .find(|pr| pr.head_commit() == branch.local_oid)
        }
        0 => None,
        1 => requests.open.pop(),
        _ => return Status::Multiple(requests.open),
    };
    match pr {
        Some(pr) => Status::Found {
            branch,
            head: head.to_owned(),
            pr: Box::new(pr),
        },
        None => Status::Create {
            branch,
            unavailable: None,
        },
    }
}

fn github_api_origin() -> String {
    #[cfg(all(feature = "qualification", target_os = "macos"))]
    if std::env::var_os("HUGGLER_UI_SCRIPT").is_some()
        && let Ok(origin) = std::env::var("HUGGLER_QUALIFICATION_GITHUB_API")
        && let Ok(url) = url::Url::parse(&origin)
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
    {
        return origin;
    }
    "https://api.github.com".into()
}

async fn github_authorization() -> Result<String> {
    let run = |executable: &Path| {
        let mut command = Command::new(executable);
        command
            .args(["auth", "token", "--hostname", "github.com"])
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env_remove("GH_DEBUG")
            .env_remove("DEBUG")
            .kill_on_drop(true)
            .stdin(Stdio::null());
        async move {
            tokio::time::timeout(Duration::from_secs(10), command.output())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GitHub CLI timed out"))?
        }
    };
    let output = match run(Path::new("gh")).await {
        // Finder-launched apps can have a system-only PATH.
        Err(error) if cfg!(target_os = "macos") && error.kind() == io::ErrorKind::NotFound => {
            let executable = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]
                .into_iter()
                .find(|path| Path::new(path).is_file())
                .context("Install GitHub CLI to show pull request status.")?;
            run(Path::new(executable)).await
        }
        result => result,
    }
    .context("Read GitHub CLI credentials")?;
    // Never include CLI output in an error or persist the credential.
    ensure!(
        output.status.success() && !output.stdout.is_empty(),
        "Sign in with GitHub CLI to show pull request status."
    );
    let token = std::str::from_utf8(&output.stdout)
        .ok()
        .map(str::trim)
        .filter(|token| !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()))
        .context("GitHub CLI returned an invalid credential")?;
    Ok(format!("Bearer {token}"))
}
