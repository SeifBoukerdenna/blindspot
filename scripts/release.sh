#!/bin/bash
# Releases Blindspot in one command. Run it yourself, from main:
#
#   scripts/release.sh           the Makefile VERSION if it has no tag yet, otherwise the next patch
#   scripts/release.sh minor     0.2.8 → 0.3.0 (also: patch, major)
#   scripts/release.sh 1.0.0     an exact version
#
# It sets VERSION, adds release notes to docs/new-features.md from the commit messages when the
# guide has none for that version, runs the checks CI runs, and asks once before it commits,
# tags and pushes. The Release workflow then builds and publishes the GitHub release; this
# script follows the run and prints the release link.
#
# Options: --yes (no questions), --no-checks (skip the local checks), --no-watch (exit after pushing)
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

assume_yes=0 checks=1 watch=1 target=auto
for argument in "$@"; do
    case $argument in
        --yes) assume_yes=1 ;;
        --no-checks) checks=0 ;;
        --no-watch) watch=0 ;;
        patch | minor | major) target=$argument ;;
        [0-9]*.[0-9]*.[0-9]*) target=$argument ;;
        -h | --help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument '$argument' (see --help)" ;;
    esac
done

# Asks before committing; --yes answers yes.
confirm() { [[ $assume_yes == 1 ]] && return 0; read -r -p "$1 [y/N] " reply; [[ $reply == [yY]* ]]; }
# Offers something optional; --yes answers no.
offer() { [[ $assume_yes == 1 ]] && return 1; read -r -p "$1 [y/N] " reply; [[ $reply == [yY]* ]]; }

cd "$(git rev-parse --show-toplevel)"
guide=docs/new-features.md

[[ $(git rev-parse --abbrev-ref HEAD) == main ]] || die "switch to main first"
# Only the files this script edits may be dirty, so a failed run can simply be re-run.
unrelated=$(git status --porcelain --untracked-files=no | grep -vE "^.. (Makefile|$guide)$" || true)
[[ -z $unrelated ]] || die "commit or stash these first; a release commit holds only the version and notes:
$unrelated"

echo "Fetching origin…"
git fetch --quiet --tags origin main
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] ||
    die "main differs from origin/main; pull or push first"

current=$(make -s version)
[[ $current =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || die "Makefile VERSION '$current' is not X.Y.Z"
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
tagged() { git rev-parse -q --verify "refs/tags/v$1" >/dev/null; }
case $target in
    auto) if tagged "$current"; then version="$major.$minor.$((patch + 1))"; else version=$current; fi ;;
    patch) version="$major.$minor.$((patch + 1))" ;;
    minor) version="$major.$((minor + 1)).0" ;;
    major) version="$((major + 1)).0.0" ;;
    *) version=$target ;;
esac
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z, not '$version'"
tag="v$version"
! tagged "$version" || die "$tag already exists"
! git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1 || die "$tag already exists on GitHub"
echo "Releasing $tag (Makefile VERSION was $current)"

if [[ $version != "$current" ]]; then
    sed -i '' -E "s/^(VERSION[[:space:]]*:=[[:space:]]*).*/\1$version/" Makefile
    [[ $(make -s version) == "$version" ]] || die "could not set VERSION in the Makefile"
fi

if ! grep -qx "\*\*$version changes\*\*" "$guide"; then
    since=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
    subjects=$(git log --no-merges --format='- %s' ${since:+"$since..HEAD"})
    python3 - "$guide" "$version" "$subjects" <<'PY'
import re
import sys

path, version, subjects = sys.argv[1], sys.argv[2], sys.argv[3].strip() or "- Maintenance release."
text = open(path, encoding="utf-8").read()
first = re.search(r"^\*\*\d+\.\d+\.\d+ changes\*\*$", text, re.M)
if not first:
    sys.exit("error: the guide has no **X.Y.Z changes** entry to place the new one before")
text = text[:first.start()] + f"**{version} changes**\n\n{subjects}\n\n" + text[first.start():]
open(path, "w", encoding="utf-8").write(text)
PY
    echo
    echo "No release notes for $version yet, so these were added to $guide from the commits:"
    echo "$subjects" | sed 's/^/  /'
    if offer "Edit them before releasing?"; then
        "${EDITOR:-nano}" "$guide"
    fi
fi
# Hand-written notes still need the guide's header to name the release it describes.
python3 - "$guide" "$version" <<'PY'
import re
import sys

path, version = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
updated = re.sub(r"^\*\*Release: [0-9.]+\.\*\*", f"**Release: {version}.**", text, count=1, flags=re.M)
updated = re.sub(r"(and )\d+\.\d+\.\d+( release notes)", rf"\g<1>{version}\g<2>", updated, count=1)
if updated != text:
    open(path, "w", encoding="utf-8").write(updated)
PY
python3 scripts/release-notes.py "$guide" "$version" >/dev/null

if [[ $checks == 1 ]]; then
    echo
    echo "Running the checks CI runs (skip with --no-checks)…"
    make check-header check test test-actions
fi

echo
git --no-pager diff --stat
confirm "Commit, tag $tag and push to GitHub?" ||
    die "stopped; nothing was committed (edits to Makefile and $guide are kept)"
git add Makefile "$guide"
git diff --cached --quiet || git commit --quiet -m "Release $version"
git tag -a "$tag" -m "Blindspot $version"
git push --atomic origin main "$tag" ||
    die "push failed; $tag and the release commit exist locally. Retry: git push --atomic origin main $tag"
echo "Pushed $tag."

if [[ $watch == 0 ]] || ! command -v gh >/dev/null; then
    echo "The Release workflow is building it; the release appears on GitHub's Releases page when it finishes."
    exit 0
fi
repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
commit=$(git rev-parse "$tag^{commit}")
echo "Waiting for the Release workflow to start…"
run=""
for _ in $(seq 1 36); do
    run=$(gh run list --repo "$repo" --workflow release.yml --commit "$commit" --limit 1 \
        --json databaseId --jq '.[0].databaseId // empty' 2>/dev/null || true)
    [[ -n $run ]] && break
    sleep 5
done
[[ -n $run ]] || die "the Release workflow did not start; see https://github.com/$repo/actions"
gh run watch "$run" --repo "$repo" --exit-status >/dev/null ||
    die "the Release workflow failed; see: gh run view $run --repo $repo --log-failed"
gh release view "$tag" --repo "$repo" --json url --jq '"Released: " + .url'
