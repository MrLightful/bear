# Verify desktop changes

## macOS desktop checks

Use Xcode, Python 3.12 or later, and a macOS GUI session. Run the commands from the Bear checkout. Build the qualification binary once for the [desktop coding checks](desktop-coding.md#reproduce-the-desktop-check) and the history check.

```sh
cargo fmt --manifest-path Cargo.toml -- --check
cargo clippy --locked --manifest-path Cargo.toml --features qualification --all-targets -- -D warnings
cargo build --locked --manifest-path Cargo.toml --features qualification --target-dir target/desktop
python3 scripts/check-desktop-history.py target/desktop/debug/bear
```

The response image check uses a scripted Codex app-server and a local HTTP fixture. It verifies previews while streaming and after reopening history, fallback for missing and out-of-checkout files, and user-initiated web loading.

```sh
python3 scripts/check-desktop-images.py target/desktop/debug/bear
```

The Mermaid check verifies the open-fence source view, rendering after the fence closes, the source toggle and clipboard, malformed-diagram fallback, and saved history at a narrow width.

```sh
python3 scripts/check-desktop-mermaid.py target/desktop/debug/bear
```

The merged-PR settlement check drives the desktop with a local GitHub fixture. It verifies selected and background conversations, the persisted General setting, closed PRs, and edits or branch switches during a status request. It tracks each app PID and verifies exit.

```sh
python3 scripts/check-desktop-pull-requests.py target/desktop/debug/bear --settlement-only
```

### Capture a desktop screenshot baseline

Keep the macOS session unlocked and grant Screen Recording access to the process
hosting the check. Run the suite with the qualification binary built above.

```sh
python3 scripts/check-desktop-screenshots.py target/desktop/debug/bear .context/desktop-baseline
```

The suite drives the real window with an isolated checkout, preferences, and a
recorded provider fixture. It captures the starter and conversation setup, every
Settings page, project, branch and model pickers, the conversation and attention
states, file tree and changes panel, document and diff views, terminal, MCP status,
and image and PDF attachments. It also saves standalone component crops. Use
`--appearance light` for a separate light-mode baseline.

After a change, rebuild the qualification binary and compare a new capture with
the baseline.

```sh
python3 scripts/check-desktop-screenshots.py target/desktop/debug/bear \
  .context/desktop-current --baseline .context/desktop-baseline
```

Use an empty output directory each time. The comparison reports added, removed
and changed images and exits with status 1 if any differ. Compare captures using
the same macOS version, display scale, time zone and appearance. The qualification
build fixes transcript timestamps and durations during capture and hides the
blinking input caret. The comparison uses exact decoded pixel values. Use
`--check-only` in place of the output directory to exercise the scene checkpoints
and validate component bounds without Screen Recording access.

To add a scene, put a named `shot` in the script after the actions that open it
and supply a visible control as the readiness check. Pass `component=` with an
observed control name to save just its bounds. Register a new control with
`control!` or use `observe_bounds` for custom-painted components. See [the screenshot suite](../scripts/check-desktop-screenshots.py).

### Run provider and installation checks

The peer and recorded permission checks build and install their own temporary qualification bundles. Set up the release prerequisites below before running them.

```sh
python3 scripts/check-desktop-peers.py
python3 scripts/check-permission.py recorded-codex
```

For live checks, use an authenticated native Codex CLI on PATH or at `HUGGLER_CODEX`. The restart check verifies continuation after quit and reopen. To verify an upgrade, pass `--previous-app` with a signed qualification bundle with the same bundle identifier and a lower build number. The script works on a temporary installation.

```sh
python3 scripts/check-desktop.py
python3 scripts/check-permission.py codex
```

## Package the desktop app

On macOS, install the Rust license documentation and notice generator, fetch the locked desktop dependencies, then run the production packaging check in a GUI session.

```sh
rustup component add rust-docs
cargo install --locked cargo-about --version 0.9.1 --features cli
cargo fetch --locked --manifest-path Cargo.toml
python3 scripts/check-desktop-release.py target/distribution
```

The checker creates the app ZIP, verifies the bundle signature and launches the extracted app with a fresh home and a system-only PATH. The 0.0.1 check ran on an Apple Silicon development Mac. Qualification on a separate clean Mac, the macOS deployment floor and Gatekeeper assessment of a notarized download remain open. [Production checker](../scripts/check-desktop-release.py) · [0.0.1 qualification](https://github.com/MrLightful/huggler/blob/ef2ffe78541842fe3b13d7468e8fd330b0e674d4/docs/stage-7.md)

Run **Release candidates** from the GitHub Actions tab to download the ad-hoc signed macOS app ZIP. The macOS job uses `--package-only`. Run the command above in a macOS GUI session to also verify launch, database creation, and quit. [Release candidates workflow](../.github/workflows/release-candidates.yml)
