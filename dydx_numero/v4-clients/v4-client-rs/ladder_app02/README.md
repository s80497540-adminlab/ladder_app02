# Ladder app live feed guide

This branch adds a 24/7 dYdX data ingestion flow so the UI can hydrate immediately with live order book and trade data.

## What was added
- `data_daemon02` binary: background worker that connects to dYdX mainnet websockets, keeps reconnecting, and writes rolling snapshots/logs of order book tops and trades to disk.
- Feed bridge in the UI: on startup the app reads the cached snapshots/logs and then tails the daemon's live updates; if the daemon is not running it falls back to the deterministic dummy feed.
- Shared persistence structs (`src/feed_shared.rs` and `src/feed/daemon.rs`): keep the daemon and UI aligned on file locations and data formats.

## How to run the always-on daemon
1. Build the release binary:
   ```sh
   cargo build --release --bin data_daemon02
   ```
2. (macOS) Install it as a launchd agent for automatic start/keep-alive:
   ```sh
   ./install_data_daemon02.zsh
   ```
   This writes `~/Library/LaunchAgents/com.ladder.data_daemon02.plist` and logs to `daemon02_stdout.log` / `daemon02_stderr.log`.
3. Verify it's running (macOS):
   ```sh
   launchctl list | grep com.ladder.data_daemon02
   ```
4. To run manually instead of launchd:
   ```sh
   target/release/data_daemon02
   ```

## Using the UI with live data
1. Start the daemon (see above) and let it run continuously; it will keep caching data even when the UI is closed.
2. Launch the app normally (e.g., `cargo run -p ladder_app02`). On startup it loads cached snapshots/logs so the ladder and charts populate immediately, then continues streaming fresh updates.
3. If the daemon is not available, the UI automatically falls back to the built-in dummy feed so the app still renders.

## What “create PR” means
A pull request (PR) is a review bundle of commits. The `make_pr` step in this repo prepares the PR title/body describing the changes so they can be reviewed and merged. After committing your changes locally, run `make_pr` with a concise title/summary to draft the PR message.

## If GitHub shows merge conflicts
GitHub is flagging conflicts in `src/bin/data_daemon02.rs` and `src/feed/daemon.rs`. That means the base branch changed in those files after this branch was created. To resolve:

1. Fetch the latest default branch (usually `main`) and either rebase or merge it into this branch.
2. Open the reported files and clear any `<<<<<<<`, `=======`, `>>>>>>>` markers by choosing/merging the correct code paths.
3. Run `cargo check` (or `cargo test`) to ensure the resolved code compiles.
4. Commit the conflict fixes and push; GitHub will clear the conflict banner once the branch contains the resolved files.

If you share the current `main` contents of those two files, I can integrate them directly into this branch and push the resolved version for you.
