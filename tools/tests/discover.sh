#!/usr/bin/env bash
# Output one record per selected guest and build profile.
set -euo pipefail
if (( $# > 2 )); then
    echo "usage: discover.sh [repository] [base]" >&2
    exit 2
fi
root=$(cd "${1:-.}" && pwd)
source "$(dirname "$0")/changes.sh"
changed_paths "$root" "${2:-}"

for path in "${changed[@]}"; do
    case "$path" in
        rust-toolchain.toml) full=true ;;
        Cargo.toml|Cargo.lock|.cargo/*|server/Cargo.toml|server/build.rs|server/tests/qemu.version|server/tests/runner/*|tools/tests/discover.sh|tools/tests/changes.sh|tools/tests/batch.sh|.github/workflows/target.yml)
            full=true ;;
    esac
done

matches() {
    local path=$1 pattern
    shift
    for pattern in "$@"; do
        [[ $path != $pattern ]] || return 0
    done
    return 1
}

declare -A ids=()
records=()
while IFS= read -r -d '' file; do
    suite=${file%/suite.json}
    suite=${suite#"$root"/}
    id=$(jq -er '.id | select(test("^[a-z0-9-]+$"))' "$file")
    [[ ! -v "ids[$id]" ]] || { echo "duplicate suite: $id" >&2; exit 1; }
    ids[$id]=1
    jq -e '
        (.profiles | type == "array" and length > 0 and all(.[]; . == "debug" or . == "release"))
        and (.sources | type == "array" and length > 0)
        and (if .run.kind == "board-boot" then
            (.run.boards | length > 0 and all(.[]; . == "q35" or . == "virt"))
            and (((.run.board_sources // {}) | keys) - .run.boards == [])
        else .run.kind == "qemu" or .run.kind == "cargo-test" end)' "$file" >/dev/null
    mapfile -t common < <(jq -r '.sources[], (.run.imports[]? | "server/tests/" + . + "/**")' "$file")
    mapfile -t scoped < <(jq -r '.run.board_sources[]?[]' "$file")
    mapfile -t platforms < <(jq -r 'if .run.kind == "board-boot" then .run.boards[] elif .run.kind == "qemu" then "rv64" else "host" end' "$file")
    for platform in "${platforms[@]}"; do
        selected=$full
        mapfile -t specific < <(jq -r --arg p "$platform" '.run.board_sources[$p][]?' "$file")
        for path in "${changed[@]}"; do
            if matches "$path" "${common[@]}" "${specific[@]}" ||
                { [[ $path == "$suite/"* ]] && ! matches "$path" "${scoped[@]}"; }; then
                selected=true
                break
            fi
        done
        if [[ $selected == true ]]; then
            records+=("$(jq -c --arg suite "$suite" --arg platform "$platform" '
                .id as $id | .profiles[] as $profile | {$id,$suite,$profile,$platform}' "$file")")
        fi
    done
done < <(find "$root/server/tests/target" -name target -mindepth 1 -type d -prune -o -name suite.json -type f -print0 | sort -z)
printf '%s\n' "${records[@]}" | jq -cs .
