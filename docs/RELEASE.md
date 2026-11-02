# Release process

This repository is a tracking fork of firstmate.
Each release records the upstream firstmate commit it is based on and ships under a semantic version tag.
The `UPSTREAM` file at the repository root is the only record of that upstream base.
A maintainer follows this procedure; agents and crewmates never create or push release tags.

## Prerequisites

- Git 2.23 or newer.
- Push access to `origin` and maintainer rights on this repository.
- A clean `main` that matches `origin/main`, with no uncommitted changes.

## Remotes

- `origin` is this repository.
- `upstream` is the source firstmate repository, `https://github.com/kunchenguid/firstmate`.

Add the upstream remote once in each maintainer clone:

```sh
git remote add upstream https://github.com/kunchenguid/firstmate
git fetch upstream
git remote -v
```

`git remote -v` must list both `origin` and `upstream`.

## The UPSTREAM record

`UPSTREAM` holds one full 40-character upstream firstmate commit SHA.
It has no trailing newline and no other text.
It names the upstream commit that the release's contents are based on.
Only a release changes it; ordinary development does not.

Confirm that the recorded commit is part of upstream history:

```sh
git merge-base --is-ancestor "$(cat UPSTREAM)" upstream/main
```

Write the value with `printf`, because `git rev-parse` output ends with a newline:

```sh
printf '%s' '<full-upstream-sha>' > UPSTREAM
```

## Release steps

1. Switch to `main` and fast-forward it: `git switch main` then `git pull --ff-only origin main`.
2. Fetch upstream: `git fetch upstream`.
3. Decide the upstream base.
   Keep the previous `UPSTREAM` value when no upstream commits were merged since the last release.
   If upstream commits were merged since then, record the newest upstream commit whose changes are in `main`.
   The upstream merge workflow is not defined for the alpha, so record whatever upstream commit the release actually contains.
4. Write the `UPSTREAM` value as shown above.
5. Commit the record with the release message:

   ```sh
   git add UPSTREAM
   git commit -m "Release vX.Y.Z: upstream <short-SHA>"
   ```

   `<short-SHA>` is the first eight characters of the value in `UPSTREAM`.
   Every release has its own commit, so use `git commit --allow-empty` when the value did not change.
6. Create an annotated tag on that commit:

   ```sh
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   ```

7. Push in this order:

   ```sh
   git push origin main &&
     git push origin vX.Y.Z
   ```

   Push the commit before its tag so the tag never points at a commit that `origin` lacks.
   Push the release tag by name; `git push --tags` pushes every local tag.
8. Tell the ai-local-setup maintainers the new tag.
   They set `MOA_VERSION` to `vX.Y.Z` in the install-mc-multi-agent skill; this repository does not edit that repository.
9. Verify the release:

   ```sh
   git cat-file -t vX.Y.Z
   git show vX.Y.Z:UPSTREAM
   git describe --tags --exact-match "vX.Y.Z^{commit}"
   ```

   `git cat-file -t` must print `tag`, which proves the tag is annotated.

## Versioning

- Tags use the form `vMAJOR.MINOR.PATCH` and follow semantic versioning.
- The first release is `v0.1.0`, the alpha.
- Compare the changes since the previous tag and choose the highest level any of them requires.
- Bump PATCH for bug fixes only.
- Bump MINOR for new features.
- Bump MAJOR for breaking changes.
- Pre-release and build suffixes are not used during the alpha.
- Tags are immutable: never move or re-create a published tag.

## Rollback

Use these steps for a release cut by mistake.
Never rewrite history or force-push `main`.

1. If the tag has not been handed to ai-local-setup yet, delete it locally and on `origin`:

   ```sh
   git tag -d vX.Y.Z
   git push origin --delete vX.Y.Z
   ```

2. If the release commit changed `UPSTREAM`, undo it with a new commit, then push `main`.
   If the release commit is empty, skip this step; there is nothing to revert.

   ```sh
   git revert <release-commit>
   git push origin main
   ```

3. If ai-local-setup may already have pinned the tag, do not delete it.
   Fix the problem in a new commit and cut the next version instead, for example `v0.1.1`.
