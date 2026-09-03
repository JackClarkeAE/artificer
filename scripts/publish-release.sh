#!/usr/bin/env bash
#
# Publish a release to main, and repair release tags that point off main.
#
# Two jobs, either of which can be run alone:
#
#   publish  Fetch, merge the development branch into main, run the gates the
#            CI runs, push main, then create and push the release tag.
#   retag    Re-point every tag whose commit is not reachable from main at the
#            commit on main that carries the identical tree.
#
# The retag job exists because main's history was rebuilt at some point: the
# old tags still name real commits, but those commits are orphaned twins of
# the ones on main — same tree, same author, same date, different parents. A
# clone therefore fetches tags that lead nowhere. Matching by tree hash makes
# the repair mechanical rather than a guess, and the script refuses to move a
# tag whose subject does not also agree.
#
# Nothing is pushed until every gate has passed and you have confirmed. Run
# with --dry-run first; it prints every command it would run and touches
# nothing.
#
# Usage:
#   scripts/publish-release.sh publish --version 0.97 --from <branch> [options]
#   scripts/publish-release.sh retag [options]
#   scripts/publish-release.sh all --version 0.97 --from <branch> [options]
#
# Options:
#   --repo <path>     Repository to work in. Default: this script's repository.
#   --remote <name>   Default: origin.
#   --main <branch>   Default: main.
#   --version <x.yz>  Release number without the leading v, e.g. 0.97.
#   --from <branch>   Development branch to merge into main.
#   --dry-run         Print what would happen; change nothing, push nothing.
#   --skip-gates      Do not run fmt/clippy/tests. Only for a re-run whose
#                     gates you have already watched pass.
#   --yes             Do not prompt. For use in a pipeline, not by hand.
#
set -euo pipefail

REMOTE=origin
MAIN=main
REPO=""
VERSION=""
FROM=""
DRY_RUN=0
SKIP_GATES=0
ASSUME_YES=0
JOB=""

die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
note() { printf '\033[36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }

# Every mutating command goes through run(), so --dry-run is honoured in one
# place rather than at each call site.
run() {
  if (( DRY_RUN )); then
    printf '  \033[90mwould run:\033[0m %s\n' "$*"
  else
    printf '  \033[90m$\033[0m %s\n' "$*"
    "$@"
  fi
}

confirm() {
  (( ASSUME_YES )) && return 0
  (( DRY_RUN )) && return 0
  local reply
  read -r -p "$1 [y/N] " reply
  [[ $reply == [yY] ]] || die "stopped at your request"
}

# ---------------------------------------------------------------- arguments

[[ $# -gt 0 ]] || die "no job given; expected publish, retag or all"
JOB=$1; shift
case $JOB in
  publish|retag|all) ;;
  -h|--help) sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//;$d'; exit 0 ;;
  *) die "unknown job '$JOB'; expected publish, retag or all" ;;
esac

while [[ $# -gt 0 ]]; do
  case $1 in
    --repo)    REPO=${2:?--repo needs a path}; shift 2 ;;
    --remote)  REMOTE=${2:?--remote needs a name}; shift 2 ;;
    --main)    MAIN=${2:?--main needs a branch}; shift 2 ;;
    --version) VERSION=${2:?--version needs a number}; shift 2 ;;
    --from)    FROM=${2:?--from needs a branch}; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --skip-gates) SKIP_GATES=1; shift ;;
    --yes)     ASSUME_YES=1; shift ;;
    *) die "unknown option '$1'" ;;
  esac
done

if [[ $JOB != retag ]]; then
  [[ -n $VERSION ]] || die "--version is required for '$JOB' (e.g. --version 0.97)"
  [[ -n $FROM ]]    || die "--from is required for '$JOB' (the development branch)"
  [[ $VERSION == v* ]] && die "--version takes the number without the v, e.g. 0.97"
fi
TAG="v${VERSION}"

# ------------------------------------------------------- change directory

# Default to the repository this script lives in, so the script works when
# invoked by an absolute path from anywhere.
if [[ -z $REPO ]]; then
  REPO=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
fi
[[ -d $REPO ]] || die "no such directory: $REPO"
cd -- "$REPO"
git rev-parse --git-dir >/dev/null 2>&1 || die "$REPO is not a git repository"
REPO=$(git rev-parse --show-toplevel)
cd -- "$REPO"
note "working in $REPO"

git remote get-url "$REMOTE" >/dev/null 2>&1 || die "no remote named '$REMOTE'"

if [[ -n $(git status --porcelain) ]]; then
  git status --short
  die "working tree is not clean; commit or stash first"
fi

STARTING_REF=$(git symbolic-ref --quiet --short HEAD || git rev-parse HEAD)
restore() { git checkout --quiet "$STARTING_REF" 2>/dev/null || true; }
trap restore EXIT

# ---------------------------------------------------------------- fetching

note "fetching $REMOTE"
# --prune-tags with --tags makes the local tag set match the remote's, so a
# stale local tag cannot be mistaken for the published one.
run git fetch --prune --prune-tags --tags "$REMOTE"
run git fetch "$REMOTE" "$MAIN"

MAIN_REF="refs/remotes/$REMOTE/$MAIN"
git show-ref --verify --quiet "$MAIN_REF" || die "$REMOTE/$MAIN does not exist"

# ------------------------------------------------------------------ gates

gates() {
  if (( SKIP_GATES )); then
    warn "skipping gates at your request"
    return 0
  fi
  note "running the gates CI runs"
  run cargo fmt --all --check
  run bash scripts/check-architecture-boundaries.sh
  run cargo clippy --workspace --all-targets -- -D warnings
  run cargo test --workspace

  # addons/scan is a standalone workspace with its own lockfile: no root
  # workspace job compiles it, so a version bump does not reach it and it
  # drifts silently. CI has a job for exactly this. The lockfile check must
  # come after the test run, because `cargo test` relocks in place and would
  # otherwise hide the drift.
  note "running the scan addon's gates"
  ( cd addons/scan
    run cargo fmt --all --check
    run cargo clippy --workspace --all-targets -- -D warnings
    run cargo test --workspace --all-targets
    if (( ! DRY_RUN )) && ! git diff --exit-code Cargo.lock; then
      die "addons/scan/Cargo.lock is out of date. Fix with:
       cd addons/scan && cargo update -p artificer-geometry
     then commit it. This happens on every workspace version bump."
    fi
  )
}

# ---------------------------------------------------------------- publish

publish() {
  note "publishing $TAG"

  git show-ref --verify --quiet "refs/heads/$FROM" \
    || git show-ref --verify --quiet "refs/remotes/$REMOTE/$FROM" \
    || die "no branch '$FROM' locally or on $REMOTE"

  if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    die "$TAG already exists locally. Delete it first if you mean to redo the release:
       git tag -d $TAG"
  fi
  if git ls-remote --exit-code --tags "$REMOTE" "refs/tags/$TAG" >/dev/null 2>&1; then
    die "$TAG already exists on $REMOTE. A published tag is not moved casually;
     if you really mean to, delete it there first."
  fi

  # Check the version the tree declares against the one being tagged, rather
  # than trusting the argument. The tags are 0.96, 0.97 while the crates are
  # 0.9.6, 0.9.7, so "0.97" implies "0.9.7": take the digits after the dot and
  # put a dot before the last one.
  local declared expected digits
  declared=$(git show "$FROM:Cargo.toml" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -1)
  digits=${VERSION#*.}
  if [[ $VERSION == *.*.* || ${#digits} -lt 2 ]]; then
    expected=$VERSION                       # already 0.9.7, or something unusual
  else
    expected="${VERSION%.*}.${digits%${digits: -1}}.${digits: -1}"
  fi
  if [[ $declared != "$expected" ]]; then
    warn "Cargo.toml on $FROM says '$declared'; $TAG implies '$expected'."
    confirm "Publish anyway?"
  fi

  local work="release-$TAG-$$"
  note "merging $FROM into $MAIN on a scratch branch ($work)"
  run git checkout -B "$work" "$MAIN_REF"

  if (( DRY_RUN )); then
    printf '  \033[90mwould run:\033[0m git merge --no-ff %s\n' "$FROM"
  else
    if ! git merge --no-ff --no-edit "$FROM" -m "Artificer $VERSION"; then
      cat >&2 <<'EOF'

The merge conflicted. This is expected when main's release commit squashed
work the branch also carries as its own history: the two describe the same
tree by different routes.

Check whether the trees actually agree at the point they diverged. If they
do, the resolution is the branch's tree wholesale:

    git diff <main's release commit> <the branch commit it squashed>   # empty?
    git checkout <branch> -- .
    git commit

Then re-run this script with --skip-gates if you have already watched them
pass. If the trees do NOT agree, resolve by hand — do not take either side
wholesale.
EOF
      die "merge conflict; nothing has been pushed"
    fi
  fi

  gates

  note "about to push $MAIN and create $TAG"
  git --no-pager log --oneline "$MAIN_REF..HEAD" | head -20
  confirm "Push these to $REMOTE/$MAIN and tag them $TAG?"

  run git push "$REMOTE" "HEAD:refs/heads/$MAIN"
  run git tag -a "$TAG" -m "Artificer $VERSION"
  run git push "$REMOTE" "refs/tags/$TAG"

  note "published $TAG"
  run git checkout "$STARTING_REF"
  run git branch -D "$work"
}

# ------------------------------------------------------------------ retag

retag() {
  note "checking which tags point off $MAIN"

  # Build tree -> commits map for main, newest first, then match each off-main
  # tag by tree. Where several main commits share a tree (a version bump and
  # the release commit above it, say), the one whose subject also matches wins;
  # a tree match with a different subject is reported and skipped rather than
  # guessed at.
  local plan
  plan=$(MAIN_REF="$MAIN_REF" python3 - <<'PY'
import os, subprocess
main_ref = os.environ["MAIN_REF"]
def sh(*a):
    return subprocess.run(a, capture_output=True, text=True).stdout.strip()

by_tree = {}
log = sh("git", "log", "--format=%H\t%T\t%s", main_ref)
for line in log.splitlines():
    h, t, s = line.split("\t", 2)
    by_tree.setdefault(t, []).append((h, s))

for tag in sh("git", "tag", "-l").splitlines():
    commit = sh("git", "rev-list", "-n1", tag)
    on_main = subprocess.run(
        ["git", "merge-base", "--is-ancestor", commit, main_ref]
    ).returncode == 0
    if on_main:
        continue
    tree = sh("git", "rev-parse", f"{commit}^{{tree}}")
    subject = sh("git", "log", "-1", "--format=%s", commit)
    candidates = by_tree.get(tree, [])
    exact = [h for h, s in candidates if s == subject]
    if exact:
        print(f"MOVE\t{tag}\t{commit}\t{exact[0]}\t{subject}")
    elif candidates:
        print(f"AMBIGUOUS\t{tag}\t{commit}\t{candidates[0][0]}\t{subject}")
    else:
        print(f"ORPHAN\t{tag}\t{commit}\t-\t{subject}")
PY
)

  if [[ -z $plan ]]; then
    note "every tag is reachable from $MAIN; nothing to do"
    return 0
  fi

  local moves=0 skips=0
  printf '\n%-9s %-9s %-9s %s\n' "tag" "from" "to" "commit subject"
  while IFS=$'\t' read -r kind tag old new subject; do
    [[ -z $kind ]] && continue
    case $kind in
      MOVE)      printf '%-9s %-9s %-9s %s\n' "$tag" "${old:0:8}" "${new:0:8}" "${subject:0:52}"; moves=$((moves+1)) ;;
      AMBIGUOUS) printf '%-9s %-9s %-9s %s\n' "$tag" "${old:0:8}" "?" "tree matches, subject does not — skipping"; skips=$((skips+1)) ;;
      ORPHAN)    printf '%-9s %-9s %-9s %s\n' "$tag" "${old:0:8}" "-" "no commit on $MAIN has this tree — leaving alone"; skips=$((skips+1)) ;;
    esac
  done <<< "$plan"
  printf '\n'

  (( skips )) && warn "$skips tag(s) left untouched; they need a person to decide"
  if (( moves == 0 )); then
    note "nothing safe to move"
    return 0
  fi

  cat <<EOF
Moving a published tag rewrites what a release points at. Anyone who has
already fetched these keeps the old target until they fetch with --prune-tags,
and a GitHub Release attached to a tag follows the tag to its new commit.
The trees are identical, so what the release contains does not change — only
which commit object names it.
EOF
  confirm "Move $moves tag(s) on $REMOTE?"

  while IFS=$'\t' read -r kind tag old new subject; do
    [[ $kind == MOVE ]] || continue
    # Preserve the tag's kind and message: an annotated tag is recreated
    # annotated with its original message, a lightweight one stays lightweight.
    if [[ $(git cat-file -t "$tag") == tag ]]; then
      local message
      message=$(git tag -l --format='%(contents)' "$tag")
      [[ -n $message ]] || message="Artificer $tag"
      run git tag -f -a "$tag" "$new" -m "$message"
    else
      run git tag -f "$tag" "$new"
    fi
    run git push --force "$REMOTE" "refs/tags/$tag"
  done <<< "$plan"

  note "moved $moves tag(s)"
}

# -------------------------------------------------------------------- main

case $JOB in
  publish) publish ;;
  retag)   retag ;;
  all)     publish; retag ;;
esac

note "done"
