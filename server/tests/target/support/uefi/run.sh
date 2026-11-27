#!/usr/bin/env bash
set -euo pipefail
fixture=$(cd "$(dirname "$0")" && pwd)
board=${1:?expected q35 or virt}
profile=${2:?expected debug or release}
out=${3:?expected fresh evidence directory}
harts=${4:?expected runner CPU count}
scenario=${5:?expected scenario identifier}
manifest=${6:?expected suite manifest}
project=$(dirname "$manifest")

mkdir -p "$(dirname "$out")"
mkdir "$out"
out=$(cd "$out" && pwd)
verdict=inconclusive
write_verdict() {
    jq -n --arg verdict "$verdict" --arg scenario "$scenario" --arg board "$board" \
        '{verdict: $verdict, scenario: $scenario, board: $board}' > "$out/verdict.json"
}
build=$(mktemp -d)
export CARGO_TARGET_DIR=$build
trap 'write_verdict; rm -rf "$build"' EXIT
case "$profile" in
    debug) flags=() ;;
    release) flags=(--release) ;;
    *) exit 2 ;;
esac
case "$board" in
    q35)
        target=x86_64-unknown-uefi
        qemu=${QEMU_X86_64:-qemu-system-x86_64}
        firmware=${OVMF_DIR:-/usr/share/OVMF}
        code=$firmware/OVMF_CODE_4M.fd
        vars=$firmware/OVMF_VARS_4M.fd
        image=BOOTX64.EFI
        machine=q35
        expected_exit=33 ;;
    virt)
        target=aarch64-unknown-uefi
        qemu=${QEMU_AARCH64:-qemu-system-aarch64}
        firmware=${AAVMF_DIR:-/usr/share/AAVMF}
        code=$firmware/AAVMF_CODE.fd
        vars=$firmware/AAVMF_VARS.fd
        image=BOOTAA64.EFI
        machine=virt,gic-version=3,secure=off,virtualization=off,acpi=off
        expected_exit=0 ;;
    *) exit 2 ;;
esac
jq -e --arg board "$board" --arg id "$scenario" '
    def word: type == "string" and test("^[A-Za-z0-9][A-Za-z0-9_.-]*$");
    def value: type == "string" and test("^[A-Za-z0-9_.:+-]+$");
    def line: type == "string" and length > 0 and (test("[\r\n]") | not);
    def options: type == "object" and all(to_entries[]; (.key | word) and (.value | value));
    .id == $id and (.run.launch[$board] |
        ((.machine_options // {}) | options)
        and ((.devices // []) | type == "array" and all(.[]; (.name | word) and ((.options // {}) | options)))
        and (((.secondary_serial // false) | type) == "boolean")
        and ($board == "virt" or (.secondary_serial // false) == false)
        and ((.kernel // $id) | word)
        and (.expects | type == "array" and length > 0 and all(.[]; line))
        and ((.forbids // []) | type == "array" and all(.[]; line)))
' "$manifest" >/dev/null
launch=$(jq -c --arg board "$board" '.run.launch[$board]' "$manifest")
kernel=$(jq -r --arg id "$scenario" '.kernel // $id' <<<"$launch")
secondary_serial=$(jq -r '.secondary_serial // false' <<<"$launch")
machine+=$(jq -r '(.machine_options // {}) | to_entries | map(",\(.key)=\(.value)") | join("")' <<<"$launch")
mapfile -t devices < <(jq -r '
    .devices[]? | .name + ((.options // {}) | to_entries | map(",\(.key)=\(.value)") | join(""))
' <<<"$launch")
mapfile -t expects < <(jq -r '.expects[]' <<<"$launch")
mapfile -t forbids < <(jq -r '.forbids[]?' <<<"$launch")

"$qemu" --version | tee "$out/qemu.txt"
expected=$(cat "$fixture/../../../qemu.version")
[[ $(awk 'NR == 1 {print $4}' "$out/qemu.txt") == "$expected" ]]
RUSTFLAGS=-Dwarnings cargo build --locked --manifest-path "$project/Cargo.toml" \
    --target "$target" --bin "$kernel" "${flags[@]}"

mkdir -p "$out/esp/EFI/BOOT"
cp "$build/$target/$profile/$kernel.efi" "$out/esp/EFI/BOOT/$image"
cp --sparse=always "$vars" "$out/vars.fd"
cp "$manifest" "$out/suite.json"
sha256sum "$out/esp/EFI/BOOT/$image" "$code" "$out/vars.fd" "$out/suite.json" > "$out/images.sha256"
args=(-accel tcg -smp "$harts" -nodefaults -display none -nic none -no-reboot
    -machine "$machine"
    -drive "if=pflash,format=raw,readonly=on,file=$code"
    -drive "if=pflash,format=raw,file=$out/vars.fd")
if [[ -n ${QEMU_DATA_DIR:-} ]]; then args+=(-L "$QEMU_DATA_DIR"); fi
case "$board" in
    q35)
        args+=(-m 128M -drive "format=raw,file=fat:rw:$out/esp"
            -debugcon "file:$out/guest.log" -device isa-debug-exit,iobase=0xf4,iosize=4) ;;
    virt)
        args+=(-cpu cortex-a57 -m 256M -drive "if=none,id=esp,format=raw,file=fat:rw:$out/esp"
            -device virtio-blk-pci,drive=esp -serial "file:$out/guest.log")
        if [[ $secondary_serial == true ]]; then args+=(-serial null); fi ;;
esac
for device in "${devices[@]}"; do args+=(-device "$device"); done
args+=(-S -qmp stdio)
printf '%q ' "$qemu" "${args[@]}" > "$out/command.txt"
printf '\n' >> "$out/command.txt"

# Start paused so the terminal event is observable even for a short guest.
mkfifo "$out/monitor"
exec 3<>"$out/monitor"
rm "$out/monitor"
printf '%s\n' '{"execute":"qmp_capabilities","id":"capabilities"}' '{"execute":"cont","id":"start"}' >&3
status=0
timeout --foreground --kill-after=3s 45s "$qemu" "${args[@]}" <&3 3>&- >"$out/qmp.jsonl" 2>"$out/stderr.log" || status=$?
printf '%s\n' "$status" >"$out/exit.status"
cat "$out/stderr.log"
[[ ! -f "$out/guest.log" ]] || cat "$out/guest.log"
if [[ "$status" == 124 ]]; then
    verdict=timeout
    exit 1
fi
if [[ "$status" == 35 && "$board" == q35 ]] || { [[ -f "$out/guest.log" ]] && grep -q '^FAIL ' "$out/guest.log"; }; then
    verdict=fail
    exit 1
fi
[[ "$status" == "$expected_exit" && -s "$out/guest.log" ]]
for marker in "${forbids[@]}"; do
    if grep -Fxq -- "$marker" "$out/guest.log"; then
        verdict=fail
        exit 1
    fi
done
for marker in "${expects[@]}"; do
    [[ $(grep -Fxc -- "$marker" "$out/guest.log" || true) == 1 ]]
done
jq -es 'all(.[]; has("error") | not)
    and ([.[] | select(.id == "capabilities" and has("return"))] | length == 1)
    and ([.[] | select(.id == "start" and has("return"))] | length == 1)' "$out/qmp.jsonl" >/dev/null
if [[ "$board" == virt ]]; then
    jq -es '[.[] | select(.event == "SHUTDOWN" and .data.guest == true and .data.reason == "guest-shutdown")] | length == 1' "$out/qmp.jsonl" >/dev/null
fi
verdict=pass
