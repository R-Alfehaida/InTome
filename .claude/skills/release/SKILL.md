---
name: release
description: Cut an InTome release. Makes sure the changelog's Unreleased section describes it, bumps the version, tags with those notes, and pushes so the Release workflow builds the zip and publishes the GitHub release. Then watches that build and fixes it if it fails.
argument-hint: "[patch|minor|major|x.y.z]"
disable-model-invocation: true
---

# Release

`scripts/release.ps1` does the work. Run it; don't redo its steps by hand.

0. Check `resources/CHANGELOG.md` has a `## Unreleased` section covering everything since the last tag. The script refuses to release without one, and the app opens this file after updating, so it is the release notes users read.
   - If it is missing or behind, draft it from `git log <last tag>..HEAD`. Write the features and fixes a user will notice, in plain words, under `### New` and `### Fixed`, one bullet each, newest version at the top of the file. Merge commits that make one change, and leave out internal work (scripts, tests, refactors, reverted or fixed-again changes).
   - Show the draft and wait for the user's OK, then commit it as `Update the changelog for X.Y.Z` before step 1.
1. Preview with `powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Version "$ARGUMENTS" -DryRun`. The script rejects a leading `v`, so pass `v0.0.1` as `0.0.1`. With no argument the script bumps the patch version, or releases the current `Cargo.toml` version when no tag exists yet.
2. If the preview throws (not on `main`, uncommitted changes, behind `origin/main`, nothing to release, tag already exists), report the message and stop. Don't commit, stash, or pull to get past it.
3. Run the same command without `-DryRun`. It renames `## Unreleased` to `## X.Y.Z (date)`, commits that with the version bump as `Release vX.Y.Z`, tags with that section as the notes, and pushes `main` and the tag together.
4. Get the workflow run with `gh run list --workflow release.yml --branch vX.Y.Z --limit 1 --json databaseId,url,status`. It can take a few seconds to appear. Tell the user the tag and run URL now.
5. Watch the build until it finishes: `gh run watch <id> --exit-status --interval 30`. A build takes several minutes, so run it in the background and wait for it to exit rather than polling.
6. On success, confirm with `gh release view vX.Y.Z --json url,assets` that `InTome-X.Y.Z.zip` is attached, then reply with the tag, the notes, the run URL and the release URL.

## If the build fails

Read `gh run view <id> --log-failed` and find the cause before changing anything.

- **CI-only problem** (runner outage, network timeout, rate limit, a flaky download): `gh run rerun <id> --failed`, then watch again. The publish step uploads with `--clobber`, so a rerun is safe.
- **Problem in the repo** (compile error, `scripts/package_release.ps1`, the workflow, a missing file in `vendor/` or `resources/`): the tag already points at the broken commit, and moving a pushed tag needs the user's say-so. So fix it forward:
  1. Fix the root cause on `main`. Reproduce it locally first where you can: `cargo build --release`, `cargo test --lib`, or `powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1`.
  2. Show the user the failing log lines, the diff, and the patch version you would release, and ask before committing. Don't commit or release until they agree.
  3. Once they agree, commit the fix with a normal imperative subject, cut the patch release with step 1 onward (no argument bumps the patch), and watch that build too.
- **Tag and `Cargo.toml` disagree**: `release.ps1` should prevent this, so find out why before fixing; then cut a new patch release as above.

Stop and report after three failed builds, or sooner when the fix needs something only the user can do (repo secrets, permissions, GitHub settings, a decision to delete a tag). The report says which run failed, the log lines that show why, what you tried, and the release that is still broken on GitHub, if any.
