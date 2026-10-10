---
name: release
description: Cut an InTome release. Bumps the version, writes every commit since the last tag into an annotated tag, and pushes it so the Release workflow builds the zip and publishes the GitHub release.
argument-hint: "[patch|minor|major|x.y.z]"
disable-model-invocation: true
---

# Release

`scripts/release.ps1` does the work. Run it; don't redo its steps by hand.

1. Preview with `powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Version "$ARGUMENTS" -DryRun`. The script rejects a leading `v`, so pass `v0.0.1` as `0.0.1`. With no argument the script bumps the patch version, or releases the current `Cargo.toml` version when no tag exists yet.
2. If the preview throws (not on `main`, uncommitted changes, behind `origin/main`, nothing to release, tag already exists), report the message and stop. Don't commit, stash, or pull to get past it.
3. Run the same command without `-DryRun`. It commits `Release vX.Y.Z` when the version changes, tags with the notes, and pushes `main` and the tag together.
4. Get the workflow run with `gh run list --workflow release.yml --limit 1 --json url,status,headBranch`. It can take a few seconds to appear. Don't wait for the build to finish.
5. Reply with the tag, the notes, and the run URL.

If the workflow fails later, read `gh run view <id> --log-failed`. A CI-only problem (runner, network) is fixed by `gh run rerun <id>`; the publish step uploads with `--clobber`, so a rerun is safe. A code fix needs a new patch release, since the tag already points at the broken commit.
