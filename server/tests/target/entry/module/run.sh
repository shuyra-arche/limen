#!/usr/bin/env bash
set -euo pipefail
suite=$(cd "$(dirname "$0")" && pwd)
board=${1:?expected board}
[[ ${4:?expected runner CPU count} == 1 ]]
profile=${2:?expected profile}
out=${3:?expected fresh evidence directory}
mkdir "$out"
out=$(cd "$out" && pwd)
case "$profile" in
    debug) flags=() ;;
    release) flags=(--release) ;;
    *) exit 2 ;;
esac

esp="$out/esp"
log="$out/guest.log"
case "$board" in
    q35)
        target=x86_64-unknown-uefi
        qemu=${QEMU_X86_64:-qemu-system-x86_64}
        firmware=${OVMF_DIR:-/usr/share/OVMF}
        code=$firmware/OVMF_CODE_4M.secboot.strictnx.fd
        vars=$firmware/OVMF_VARS_4M.fd
        image=BOOTX64.EFI
        expected_exit=33
        args=(-machine q35,smm=on -m 128M
            -drive "format=raw,file=fat:rw:$esp"
            -debugcon "file:$log" -device isa-debug-exit,iobase=0xf4,iosize=4) ;;
    virt)
        target=aarch64-unknown-uefi
        qemu=${QEMU_AARCH64:-qemu-system-aarch64}
        firmware=${AAVMF_DIR:-/usr/share/AAVMF}
        code=$firmware/AAVMF_CODE.secboot.strictnx.fd
        vars=$firmware/AAVMF_VARS.fd
        image=BOOTAA64.EFI
        expected_exit=0
        args=(-machine virt,gic-version=3,secure=off,virtualization=off,acpi=off -cpu cortex-a57 -m 256M
            -drive "if=none,id=esp,format=raw,file=fat:rw:$esp"
            -device virtio-blk-pci,drive=esp -serial "file:$log") ;;
    *) exit 2 ;;
esac
"$qemu" --version | tee "$out/qemu.txt"
[[ $(awk 'NR == 1 {print $4}' "$out/qemu.txt") == "$(cat "$suite/../../../qemu.version")" ]]
[[ -r "$code" && -r "$vars" ]]

CARGO_TARGET_DIR=$(mktemp -d)
export CARGO_TARGET_DIR
trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
RUSTFLAGS=-Dwarnings cargo build --locked --manifest-path "$suite/Cargo.toml" --target "$target" "${flags[@]}"
mkdir -p "$esp/EFI/BOOT"
cp "$CARGO_TARGET_DIR/$target/$profile/module-execution.efi" "$esp/EFI/BOOT/$image"
cp --sparse=always "$vars" "$out/vars.fd"
mapfile -t modules < <(find "$CARGO_TARGET_DIR/$target/$profile/build" -name module.elf)
[[ ${#modules[@]} == 1 ]]
cp "${modules[0]}" "$out/module.elf"
sha256sum "$esp/EFI/BOOT/$image" "$out/module.elf" "$code" "$out/vars.fd" > "$out/images.sha256"

# The strict-NX firmware supplies the memory-attribute protocol.
args+=(-accel tcg -smp 1 -nodefaults -display none -nic none -no-reboot
    -drive "if=pflash,format=raw,readonly=on,file=$code"
    -drive "if=pflash,format=raw,file=$out/vars.fd")
if [[ -n ${QEMU_DATA_DIR:-} ]]; then args+=(-L "$QEMU_DATA_DIR"); fi
mkfifo "$out/monitor"
exec 3<>"$out/monitor"
rm "$out/monitor"
printf '%s\n' '{"execute":"qmp_capabilities","id":"capabilities"}' '{"execute":"cont","id":"start"}' >&3
status=0
timeout --foreground --kill-after=3s 45s "$qemu" "${args[@]}" -S -qmp stdio <&3 3>&- >"$out/qmp.jsonl" 2>"$out/stderr.log" || status=$?
cat "$out/stderr.log" "$log"
printf '%s\n' "$status" >"$out/exit.status"
[[ "$status" == "$expected_exit" ]]
grep -Fxq "PASS module-execution $board strictnx=1 private=1 permissions=1 identity=1 aliases=0 geometry=4096" "$log"
jq -es 'all(.[]; has("error") | not)
    and any(.[]; .id == "capabilities" and has("return"))
    and any(.[]; .id == "start" and has("return"))' "$out/qmp.jsonl" >/dev/null
if [[ "$board" == virt ]]; then
    jq -es 'any(.[]; .event == "SHUTDOWN" and .data.guest == true and .data.reason == "guest-shutdown")' "$out/qmp.jsonl" >/dev/null
fi
