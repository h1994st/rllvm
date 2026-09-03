# Releasing rllvm

Releases are automated. You do not bump the version, write the changelog, or
create a tag by hand.

## The agreement

**You write conventional commits. Everything else follows from them.**

| tool | owns |
|---|---|
| [release-please](https://github.com/googleapis/release-please) | version bump (`Cargo.toml`, `Cargo.lock`), `CHANGELOG.md`, the release PR |
| [cargo-dist](https://axodotdev.github.io/cargo-dist) | the git tag, the GitHub Release and its notes, the binaries, the crates.io publish |

## How a release happens

1. **You merge normal PRs into `main`** with conventional-commit titles.
2. **release-please opens a release PR** — "chore: release X.Y.Z" — and keeps it
   updated as more commits land. It contains the version bump and the CHANGELOG
   entry. Nothing is released while it sits open.
3. **You review and merge that PR.** This is the decision point: merging it
   means "ship this".
4. **cargo-dist takes over automatically.** It builds all five targets, creates
   the tag, publishes the GitHub Release with the CHANGELOG entry plus install
   instructions, and publishes to crates.io.

Editing the CHANGELOG before merging the release PR is fine and expected — it is
a normal PR.

## What your commit type does

While the version is below `1.0.0`:

| commit | bump | example |
|---|---|---|
| `feat!:` or a `BREAKING CHANGE:` footer | **minor** | 0.1.7 → 0.2.0 |
| `feat:` | patch | 0.1.7 → 0.1.8 |
| `fix:` | patch | 0.1.7 → 0.1.8 |
| `chore:` `ci:` `doc:` `refactor:` `test:` | none | |

**Mark breaking changes with `!`.** This is the one thing that cannot be
recovered after the fact. v0.1.7 removed `-c` and `-v` from the wrapper CLI — a
genuine break — but was committed as `feat:`, so it shipped as a patch. Under
this policy that would happen again silently.

If a breaking change has already been merged without the `!`, edit the version in
the release PR before merging it.

## Things that are no longer true

**Pushing a tag does not release.** cargo-dist runs on `workflow_dispatch`, not
on tag push, because whoever creates the GitHub Release owns its notes — and we
want dist's notes, which include install instructions. `git tag v0.1.8 && git
push` now produces a tag and nothing else.

**Do not bump `version` in `Cargo.toml` by hand.** release-please owns it, and a
manual edit will conflict with the release PR.

## If something goes wrong

**The release PR never appears.** release-please only reacts to commit types that
trigger a release. If everything since the last release was `chore:`/`ci:`/`doc:`,
there is nothing to release — that is correct behaviour, not a failure.

**The release PR merged but no release happened.** This is the failure worth
knowing about, because it is quiet. The workflow decides a release is due by
comparing the version in `.release-please-manifest.json` against existing tags.
Check the `release-please` workflow run for the dispatch step. To release by
hand:

```bash
gh workflow run release.yml -f tag=v0.1.8
```

**A release job hangs with no runner assigned.** Almost certainly a retired
GitHub runner label. This happened for v0.1.7: dist 0.30.0 requested `macos-13`,
which had been retired, and the jobs queued indefinitely rather than failing.
Check the requested labels, then upgrade dist:

```bash
gh api "repos/h1994st/rllvm/actions/runs/<id>/jobs" \
  --jq '.jobs[] | "\(.status) \(.labels|join(",")) \(.name)"'
```

**A release needs to be redone.** Nothing consumed a release that was never
published, so deleting the tag and release and re-dispatching is safe. If it was
published and crates.io accepted it, the version is permanently taken — bump to
the next patch instead.

## Changing the release setup

`release.yml` is **generated**. Do not edit it by hand; change
`dist-workspace.toml` and regenerate:

```bash
cargo install cargo-dist --version <version> --locked
dist generate
```

After regenerating, verify the custom crates.io job survived — it is emitted from
`publish-jobs = ["./publish-crates"]` and is easy to lose track of:

```bash
grep -n 'custom-publish-crates' .github/workflows/release.yml
dist plan --output-format=json   # check targets and runner labels
```
