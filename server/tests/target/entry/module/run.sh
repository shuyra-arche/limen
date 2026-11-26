#!/usr/bin/env bash
set -euo pipefail
suite=$(cd "$(dirname "$0")" && pwd)
[[ ${1:?expected board} == q35 && ${4:?expected runner CPU count} == 1 ]]
profile=${2:?expected profile}
out=${3:?expected fresh evidence directory}
mkdir "$out"
out=$(cd "$out" && pwd)
case "$profile" in
    debug) flags=() ;;
    release) flags=(--release) ;;
    *) exit 2 ;;
esac

qemu=${QEMU_X86_64:-qemu-system-x86_64}
firmware=${OVMF_DIR:-/usr/share/OVMF}
code=$firmware/OVMF_CODE_4M.secboot.strictnx.fd
vars=$firmware/OVMF_VARS_4M.fd
"$qemu" --version | tee "$out/qemu.txt"
[[ $(awk 'NR == 1 {print $4}' "$out/qemu.txt") == "$(cat "$suite/../../../qemu.version")" ]]
[[ -r "$code" && -r "$vars" ]]

CARGO_TARGET_DIR=$(mktemp -d)
export CARGO_TARGET_DIR
trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
RUSTFLAGS=-Dwarnings cargo build --locked --manifest-path "$suite/Cargo.toml" --target x86_64-unknown-uefi "${flags[@]}"
mkdir -p "$out/esp/EFI/BOOT"
cp "$CARGO_TARGET_DIR/x86_64-unknown-uefi/$profile/module-execution.efi" "$out/esp/EFI/BOOT/BOOTX64.EFI"
cp --sparse=always "$vars" "$out/vars.fd"
mapfile -t modules < <(find "$CARGO_TARGET_DIR/x86_64-unknown-uefi/$profile/build" -name x86_64.elf)
[[ ${#modules[@]} == 1 ]]
cp "${modules[0]}" "$out/module.elf"
sha256sum "$out/esp/EFI/BOOT/BOOTX64.EFI" "$out/module.elf" "$code" "$out/vars.fd" > "$out/images.sha256"

# Strictnx firmware supplies the memory-attribute protocol. The guest checks
# the final current root with a single CPU before it calls the module.
args=(-machine q35,smm=on -accel tcg -smp 1 -m 128M -nodefaults -display none -nic none -no-reboot
    -drive "if=pflash,format=raw,readonly=on,file=$code"
    -drive "if=pflash,format=raw,file=$out/vars.fd"
    -drive "format=raw,file=fat:rw:$out/esp"
    -debugcon "file:$out/guest.log" -device isa-debug-exit,iobase=0xf4,iosize=4)
if [[ -n ${QEMU_DATA_DIR:-} ]]; then args+=(-L "$QEMU_DATA_DIR"); fi
mkfifo "$out/monitor"
exec 3<>"$out/monitor"
rm "$out/monitor"
printf '%s\n' '{"execute":"qmp_capabilities","id":"capabilities"}' '{"execute":"cont","id":"start"}' >&3
status=0
timeout --foreground --kill-after=3s 45s "$qemu" "${args[@]}" -S -qmp stdio <&3 3>&- >"$out/qmp.jsonl" 2>"$out/stderr.log" || status=$?
cat "$out/stderr.log" "$out/guest.log"
printf '%s\n' "$status" >"$out/exit.status"
[[ "$status" == 33 ]]
grep -Fxq 'PASS module-execution q35 strictnx=1 private=1 permissions=1 identity=1 aliases=0 geometry=4096' "$out/guest.log"
jq -es 'all(.[]; has("error") | not)
    and any(.[]; .id == "capabilities" and has("return"))
    and any(.[]; .id == "start" and has("return"))' "$out/qmp.jsonl" >/dev/null
