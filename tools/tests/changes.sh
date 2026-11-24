# Source in a clean CI checkout. An absent or unavailable base selects everything.
changed_paths() {
    local root=$1 base=$2 ancestor file
    full=true
    changed=()
    [[ $base =~ ^[0-9a-f]{40}$ ]] || return 0
    ancestor=$(git -C "$root" merge-base "$base" HEAD) || return 0
    file=$(mktemp)
    if git -C "$root" diff --no-renames --name-only -z "$ancestor" HEAD >"$file"; then
        mapfile -d '' -t changed <"$file"
        full=false
    fi
    rm -f "$file"
}
