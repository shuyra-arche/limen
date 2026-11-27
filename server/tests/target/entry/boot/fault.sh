#!/usr/bin/env bash
# Boot-owned fault injection in a copy of the successful linked image.
set -euo pipefail
runner=$1
evidence=$2
plan=${PLAN:?expected discovery plan}

symbol() {
    readelf -Ws "$1" | awk -v name="$2" '$8 == name {print $2}'
}

failed=0
for profile in $(jq -r '.[] | select(.id == "boot-handoff" and .platform == "rv64") | .profile' <<<"$plan"); do
    candidate="$evidence/boot-handoff-rv64-$profile/candidate"
    elf="$candidate/platform.elf"
    normal="$candidate/harts-4-round-0/result.json"
    # Require the successful unmodified case and the exact input image.
    hash=$(sha256sum "$elf" | cut -d' ' -f1)
    jq -e --arg hash "$hash" '.sha256["platform.elf"] == $hash' "$candidate/inputs.json" >/dev/null
    jq -e --arg hash "$hash" '.verdict == "pass" and .platform_sha256 == $hash' "$normal" >/dev/null

    start=$((16#$(symbol "$elf" _start)))
    bss_start=$((16#$(symbol "$elf" _bss_start)))
    bss_end=$((16#$(symbol "$elf" _bss_end)))
    (( bss_end - bss_start > 8 ))
    # The trusted image supplies the stack. Check the same bounds the entry uses.
    major_start=$((16#$(symbol "$elf" _major_stack_start)))
    major_top=$((16#$(symbol "$elf" _major_stack_end)))
    (( bss_start <= major_start && major_start < major_top && major_top <= bss_end ))
    (( major_start % 16 == 0 && major_top % 16 == 0 ))
    IFS=' ' read -r text_address text_offset text_size < <(
        readelf -WS "$elf" | awk '{for (i=1; i<=NF; ++i) if ($i==".text" && $(i+1)=="PROGBITS") print $(i+2),$(i+3),$(i+4)}'
    )
    offset=$((16#$text_offset + start - 16#$text_address))
    (( start >= 16#$text_address && start + 128 <= 16#$text_address + 16#$text_size ))
    bytes=$(od -An -v -tx1 -j "$offset" -N 128 "$elf" | tr -d ' \n')
    # csrw stvec,t0 must precede the exact clear loop:
    # beq t0,t1,+12; sd zero,0(t0); c.addi t0,8; c.j -10.
    # Fail closed if the backend encoding changes; review the injection.
    loop=6386620023b00200a102ddbf
    [[ $bytes == *"$loop"* ]]
    prefix=${bytes%%"$loop"*}
    suffix=${bytes#*"$loop"}
    [[ $prefix == *73905210* && $suffix != *"$loop"* ]]
    (( ${#prefix} % 4 == 0 ))
    delta=$((${#prefix} / 2 + 8))
    pc=$((start + delta))
    position=$((offset + delta))
    output="$candidate/fault"
    mkdir "$output"
    fault="$output/platform.elf"
    cp "$elf" "$fault"
    printf '\000\000' | dd of="$fault" bs=1 seek="$position" conv=notrunc status=none
    patched_hash=$(sha256sum "$fault" | cut -d' ' -f1)
    printf -v pc_hex '0x%x' "$pc"
    printf -v sp_hex '0x%x' "$major_top"
    printf -v major_start_hex '0x%x' "$major_start"
    jq -n --arg source "$(jq -r .source "$candidate/inputs.json")" \
        --arg original "$hash" --arg patched "$patched_hash" \
        --arg pc "$pc_hex" --arg sp "$sp_hex" --arg start "$major_start_hex" --argjson offset "$position" \
        '{source:$source,original_sha256:$original,patched_sha256:$patched,pc:$pc,interrupted_sp:$sp,major_stack_start:$start,file_offset:$offset,before:"a102",after:"0000"}' \
        > "$output/injection.json"
    # Reuse the existing bounded QEMU/QMP executor.
    "$runner" observe "$fault" "$output/run"
    record="$output/run/observation.json"
    serial="$output/run/serial.log"
    # The pinned OpenSBI syscon-poweroff path reports guest-shutdown
    # even for SRST_SYSTEM_FAILURE. QMP proves guest termination, not the
    # SBI reason; the exact terminal diagnostic distinguishes this case.
    diagnostic="^FAIL Limen boot trap: cause=0x2 pc=$pc_hex value=0x0 stack=$sp_hex status=0x[0-9a-f]+$"
    verdict=inconclusive
    if jq -e '.status == "timeout"' "$record" >/dev/null; then
        verdict=timeout
    elif jq -e --arg hash "$patched_hash" '.status == "exited" and .code == 0 and .platform_sha256 == $hash and .qmp_shutdown == {guest:true,reason:"guest-shutdown"}' "$record" >/dev/null &&
        [[ $(tail -c1 "$serial" | od -An -tu1 | tr -d ' \n') == 10 ]] &&
        [[ $(tr -d '\r' < "$serial" | grep -Ec "$diagnostic" || :) == 1 ]] &&
        [[ $(tr -d '\r' < "$serial" | grep -Ec '^FAIL( |$)' || :) == 1 ]] &&
        ! grep -q 'PASS boot-handoff' "$serial"; then
        verdict=pass
    elif jq -e '.status == "exited"' "$record" >/dev/null; then
        verdict=fail
    fi
    jq -n --arg verdict "$verdict" '{verdict:$verdict}' > "$output/result.json"
    printf '%s boot-bss-fault-rv64-%s\n' "${verdict^^}" "$profile"
    if [[ $verdict != pass ]]; then
        failed=1
        cat "$record" >&2
        tail -n 10 "$serial" >&2
    fi
done
exit "$failed"
