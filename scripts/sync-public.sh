#!/bin/zsh
set -euo pipefail

# OpenCodeServer public-mirror snapshot sync.
#
# Publishes the current tree of this private repository to the public
# mirror repository as ONE synthetic snapshot commit. The private commit
# history never leaves this repository: the snapshot is built with
# git commit-tree from the HEAD tree and parented on the public mirror's
# main tip, so the public history is a linear chain of snapshots and the
# push is a fast-forward.
#
# Safety gates, in order:
#   1. the working tree must be clean;
#   2. the target remote must exist and must not be the private origin;
#   3. the snapshot tree must pass the sensitive-content scan:
#      - fixed strings from ~/.config/opencodeserver/sensitive-patterns
#        (case-insensitive), and
#      - generic patterns (email-looking tokens, absolute /Users/<name>
#        paths),
#      with hits suppressed by fixed strings listed in
#      ~/.config/opencodeserver/sync-allowlist.
#
# Usage:
#   scripts/sync-public.sh [--dry-run] [--note "<text>"] [--remote <name>]
#
# Absorbing external contributions merged on the public side:
#   git fetch public && git merge public/main   # run in the private repo
#
# See AGENTS.md "Public mirror discipline" for the full model.

repo_root="${0:A:h:h}"
cd "$repo_root"

remote_name="public"
config_dir="$HOME/.config/opencodeserver"
patterns_file="$config_dir/sensitive-patterns"
allow_file="$config_dir/sync-allowlist"
dry_run=0
note=""

while (( $# > 0 )); do
    case "$1" in
        --dry-run)
            dry_run=1
            shift
            ;;
        --note)
            (( $# >= 2 )) || { print -u2 "sync-public: --note requires a value"; exit 2; }
            note="$2"
            shift 2
            ;;
        --remote)
            (( $# >= 2 )) || { print -u2 "sync-public: --remote requires a value"; exit 2; }
            remote_name="$2"
            shift 2
            ;;
        *)
            print -u2 "usage: sync-public.sh [--dry-run] [--note <text>] [--remote <name>]"
            exit 2
            ;;
    esac
done

fail() {
    print -u2 "sync-public: $*"
    exit 1
}

# Gate 1: clean working tree.
git diff --quiet || fail "working tree has unstaged changes; commit or stash first"
git diff --cached --quiet || fail "index has staged changes; commit or stash first"

# Gate 2: remote configured and distinct from the private origin.
git remote get-url "$remote_name" >/dev/null 2>&1 || fail \
    "remote '$remote_name' is not configured.
  Create the (empty) public repository, then:
  git remote add $remote_name <public-repository-url>"
target_url="$(git remote get-url "$remote_name")"
origin_url="$(git remote get-url origin 2>/dev/null || true)"
[[ -z "$origin_url" || "$target_url" != "$origin_url" ]] || fail \
    "remote '$remote_name' points at the private origin; refusing to sync"

# Parent: the public mirror's main tip, so snapshots chain linearly. A
# failed fetch (empty new repository, network error) degrades to a
# parentless first snapshot; if the remote tip actually exists, the push
# is then rejected as non-fast-forward, which fails safely.
parent=""
if git fetch "$remote_name" main >/dev/null 2>&1; then
    parent="$(git rev-parse --verify --quiet "$remote_name/main" || true)"
fi

tree="$(git rev-parse 'HEAD^{tree}')"

# Tracked files that must stay maintainer-private: stripped from the
# snapshot tree before scanning and pushing. Extend this list (not the
# sync flow) when adding more private documents.
private_paths=(
    docs/release-workflow.md
)
private_index="$(mktemp)"
public_tree="$tree"
tree_files="$(git ls-tree -r --name-only "$tree")"
removed_private=()
for private_path in "${private_paths[@]}"; do
    if print -r -- "$tree_files" | /usr/bin/grep -Fxq "$private_path"; then
        removed_private+=("$private_path")
    fi
done
if (( ${#removed_private[@]} > 0 )); then
    GIT_INDEX_FILE="$private_index" git read-tree "$tree"
    GIT_INDEX_FILE="$private_index" git rm --cached -q --ignore-unmatch "${removed_private[@]}"
    public_tree="$(GIT_INDEX_FILE="$private_index" git write-tree)"
    print "excluding maintainer-private files from the snapshot:"
    for private_path in "${removed_private[@]}"; do
        print "  - $private_path"
    done
fi

# Gate 3: sensitive-content scan over the snapshot tree.
hits="$(mktemp)"
patterns="$(mktemp)"
filtered=""
trap 'rm -f "$hits" "$patterns" "$private_index" ${filtered:+"$filtered"}' EXIT
/usr/bin/grep -v '^[[:space:]]*$' "$patterns_file" >"$patterns" 2>/dev/null || true
if [[ -s "$patterns" ]]; then
    git grep -i -I -n -F -f "$patterns" "$public_tree" -- . >>"$hits" || true
else
    print -u2 "sync-public: warning: $patterns_file is missing or empty; generic patterns only"
fi
git grep -I -n -E \
    -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' \
    -e '/Users/[A-Za-z0-9._-]+' \
    "$public_tree" -- . >>"$hits" || true
if [[ -s "$hits" && -r "$allow_file" ]]; then
    while IFS= read -r allowed; do
        [[ -n "$allowed" ]] || continue
        filtered="$hits.filtered"
        /usr/bin/grep -F -v -- "$allowed" "$hits" >"$filtered" || true
        /bin/mv "$filtered" "$hits"
        filtered=""
    done <"$allow_file"
fi
if [[ -s "$hits" ]]; then
    print -u2 "sync-public: sensitive content detected in the snapshot tree:"
    /bin/cat "$hits" >&2
    print -u2 ""
    print -u2 "Refusing to sync. Remove the values from the tree, or - only for verified"
    print -u2 "false positives - add the fixed string to $allow_file."
    exit 1
fi

message="Sync snapshot from private development $(date +%Y-%m-%d)"
if [[ -n "$note" ]]; then
    message="$message

$note"
fi
if [[ -n "$parent" ]]; then
    commit="$(printf '%s\n' "$message" | git commit-tree "$public_tree" -p "$parent")"
else
    commit="$(printf '%s\n' "$message" | git commit-tree "$public_tree")"
fi

if (( dry_run )); then
    print "dry run: no push performed"
    print "  tree:   $public_tree"
    print "  parent: ${parent:-<none - would be the first snapshot>}"
    print "  commit: $commit"
    if [[ -n "$parent" ]]; then
        git diff --stat "$parent" "$commit" | /usr/bin/tail -1
    fi
    exit 0
fi

git push "$remote_name" "${commit}:refs/heads/main"
print "Synced snapshot $commit to $remote_name/main"
