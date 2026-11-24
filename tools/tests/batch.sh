#!/usr/bin/env bash
# Run the selected guests in one prepared checkout. Keep every verdict on failure.
set -euo pipefail
runner=$1
out=$2
if [[ ${3:-} == --one ]]; then
    record=$4
    IFS=$'\t' read -r id suite profile platform < <(jq -r '[.id,.suite,.profile,.platform] | @tsv' <<<"$record")
    label="$id-$platform-$profile"
    args=(run --kernel "$PWD" --suite "$suite" --profile "$profile" --output "$out/$label")
    case "$platform" in q35|virt) args+=(--board "$platform") ;; esac
    status=0
    "$runner" "${args[@]}" >"$out/$label.log" 2>&1 || status=$?
    if (( status )); then
        echo "FAIL $label" >&2
        tail -n 40 "$out/$label.log" >&2
        find "$out/$label" -type f \( -name stderr.log -o -name guest.log -o -name serial.log \) -exec tail -n 20 {} + >&2 || :
    else
        echo "PASS $label"
    fi
    jq -cn --argjson test "$record" --argjson code "$status" '$test + {exit:$code}' >"$out/$label.status"
    exit 0
fi

plan=${PLAN:?expected discovery plan}
mkdir "$out"
printf '%s\n' "$plan" >"$out/plan.json"
# Limit concurrent TCG guests on the shared machine. Cargo and QEMU output is per case.
jq -c '.[]' <<<"$plan" | xargs -r -d '\n' -P 2 -n 1 bash "$0" "$runner" "$out" --one
jq -s . "$out"/*.status >"$out/results.json"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    jq -r '.[] | "- \(.id) / \(.platform) / \(.profile) — \(if .exit == 0 then "pass" else "FAIL" end)"' "$out/results.json" >>"$GITHUB_STEP_SUMMARY"
fi
jq -e --argjson plan "$plan" 'length == ($plan|length) and all(.[]; .exit == 0)' "$out/results.json"
