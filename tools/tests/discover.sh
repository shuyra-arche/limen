#!/usr/bin/env bash
# Discover test owners; no test project name belongs in this script or CI.
set -euo pipefail

root="$(cd "${1:-.}" && pwd)"
base="${2:-}"
case "${3:-4}" in
    4|declared) ;;
    *) echo "target scenarios use four virtual CPUs" >&2; exit 1 ;;
esac
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

changed=()
filter=false
if [[ "$base" =~ ^[0-9a-f]{40}$ ]]; then
    ancestor="$(git -C "$root" merge-base "$base" HEAD 2>/dev/null || :)"
    if [[ -n "$ancestor" ]]; then
        mapfile -d '' -t changed < <(git -C "$root" diff -z --name-only "$ancestor" HEAD)
        filter=true
    fi
fi

# Infrastructure changes affect all projects; production changes affect only
# the owners that explicitly declare those source paths.
for change in "${changed[@]}"; do
    case "$change" in
        server/tests/runner/*|server/tests/qemu.version|tools/tests/discover.sh|.github/workflows/target.yml)
            filter=false
            ;;
    esac
done

declare -A ids=()
jobs=()
while IFS= read -r -d '' file; do
    directory="${file%/suite.json}"
    relative="${directory#"$root"/}"
    id="$(jq -er '.id | select(test("^[a-z0-9-]+$"))' "$file")"
    [[ ! -v "ids[$id]" ]] || { echo "duplicate suite id: $id" >&2; exit 1; }
    ids["$id"]=1
    jq -e '.profiles | type == "array" and length > 0 and all(.[]; . == "debug" or . == "release")' "$file" >/dev/null
    jq -e '.sources | type == "array" and length > 0' "$file" >/dev/null

    selected=true
    if [[ "$filter" == true ]]; then
        selected=false
        for change in "${changed[@]}"; do
            if [[ "$change" == "$relative/"* ]]; then
                selected=true
                break
            fi
            while IFS= read -r pattern; do
                if [[ "$change" == $pattern ]]; then
                    selected=true
                    break
                fi
            done < <(jq -r '.sources[]' "$file")
            [[ "$selected" == true ]] && break
        done
    fi
    [[ "$selected" == true ]] || continue

    suite_jobs="$(jq -c --arg suite "$relative" --arg id "$id" '
        .profiles[] as $profile | {suite: $suite, id: $id, profile: $profile}
    ' "$file")"
    [[ -z "$suite_jobs" ]] || jobs+=("$suite_jobs")
done < <(find "$root/server/tests/target" -name suite.json -type f -print0 | sort -z)

if (( ${#jobs[@]} == 0 )); then
    printf '[]\n'
else
    printf '%s\n' "${jobs[@]}" | jq -cs '.'
fi
