#!/usr/bin/env bash
set -euo pipefail
suite=$(cd "$(dirname "$0")" && pwd)
platform=${1:?expected q35 or virt}
profile=${2:-debug}
out=${3:?expected fresh evidence directory}
mkdir -p "$(dirname "$out")"
mkdir "$out"
out=$(cd "$out" && pwd)
case "$profile" in
    debug) flags=() ;;
    release) flags=(--release) ;;
    *) exit 2 ;;
esac
case "$platform" in
    q35) target=x86_64-unknown-uefi; qemu=${QEMU_X86_64:-qemu-system-x86_64} ;;
    virt) target=aarch64-unknown-none-softfloat; qemu=${QEMU_AARCH64:-qemu-system-aarch64} ;;
    *) exit 2 ;;
esac
"$qemu" --version | tee "$out/qemu.txt"
expected=$(cat "$suite/../../../qemu.version")
[[ $(awk 'NR == 1 {print $4}' "$out/qemu.txt") == "$expected" ]]
CARGO_TARGET_DIR=$(mktemp -d)
export CARGO_TARGET_DIR
trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
RUSTFLAGS=-Dwarnings cargo build --locked --manifest-path "$suite/Cargo.toml" --target "$target" "${flags[@]}"
bin="$CARGO_TARGET_DIR/$target/$profile/platform-boot"
args=(-accel tcg -smp 1 -m 128M -nodefaults -display none -nic none -no-reboot)
if [[ -n ${QEMU_DATA_DIR:-} ]]; then args+=(-L "$QEMU_DATA_DIR"); fi
case "$platform" in
    q35)
        firmware=${OVMF_DIR:-/usr/share/OVMF}
        mkdir -p "$out/esp/EFI/BOOT"
        cp "$bin.efi" "$out/esp/EFI/BOOT/BOOTX64.EFI"
        cp "$firmware/OVMF_VARS_4M.fd" "$out/vars.fd"
        sha256sum "$bin.efi" "$firmware/OVMF_CODE_4M.fd" "$out/vars.fd" > "$out/images.sha256"
        args+=(-machine q35 -drive "if=pflash,format=raw,readonly=on,file=$firmware/OVMF_CODE_4M.fd"
            -drive "if=pflash,format=raw,file=$out/vars.fd" -drive "format=raw,file=fat:rw:$out/esp"
            -debugcon "file:$out/guest.log" -device isa-debug-exit,iobase=0xf4,iosize=4)
        expected_exit=33
        marker='PASS q35 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1' ;;
    virt)
        host=$(rustc -vV | awk '/^host:/ {print $2}')
        objcopy="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-objcopy"
        "$objcopy" -O binary "$bin" "$out/Image"
        sha256sum "$bin" "$out/Image" > "$out/images.sha256"
        args+=(-machine virt,gic-version=3,secure=off,virtualization=off -cpu cortex-a57
            -kernel "$out/Image" -serial "file:$out/guest.log")
        expected_exit=0
        marker='PASS virt el1=1 masked=1 stack=1 vectors=1 dtb=1' ;;
esac
# Keep QMP stdin open until QEMU exits. The guest supplies the exit signal.
mkfifo "$out/monitor"
exec 3<>"$out/monitor"
rm "$out/monitor"
printf '{"execute":"qmp_capabilities"}\n' >&3
status=0
timeout --foreground --kill-after=3s 45s "$qemu" "${args[@]}" -qmp stdio <&3 3>&- >"$out/qmp.jsonl" 2>"$out/stderr.log" || status=$?
cat "$out/stderr.log" "$out/guest.log"
printf '%s\n' "$status" >"$out/exit.status"
[[ "$status" == "$expected_exit" ]]
grep -Fxq "$marker" "$out/guest.log"
if [[ "$platform" == virt ]]; then
    jq -es 'any(.[]; .event == "SHUTDOWN" and .data.guest == true and .data.reason == "guest-shutdown")' "$out/qmp.jsonl" >/dev/null
fi
