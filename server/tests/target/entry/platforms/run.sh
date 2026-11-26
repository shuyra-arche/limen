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
    q35)
        target=x86_64-unknown-uefi
        qemu=${QEMU_X86_64:-qemu-system-x86_64}
        firmware=${OVMF_DIR:-/usr/share/OVMF}
        code=$firmware/OVMF_CODE_4M.fd
        vars=$firmware/OVMF_VARS_4M.fd
        image=BOOTX64.EFI ;;
    virt)
        target=aarch64-unknown-uefi
        qemu=${QEMU_AARCH64:-qemu-system-aarch64}
        firmware=${AAVMF_DIR:-/usr/share/AAVMF}
        code=$firmware/AAVMF_CODE.fd
        vars=$firmware/AAVMF_VARS.fd
        image=BOOTAA64.EFI ;;
    *) exit 2 ;;
esac
"$qemu" --version | tee "$out/qemu.txt"
expected=$(cat "$suite/../../../qemu.version")
[[ $(awk 'NR == 1 {print $4}' "$out/qemu.txt") == "$expected" ]]
CARGO_TARGET_DIR=$(mktemp -d)
export CARGO_TARGET_DIR
trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
RUSTFLAGS=-Dwarnings cargo build --locked --manifest-path "$suite/Cargo.toml" --target "$target" "${flags[@]}"
mkdir -p "$out/esp/EFI/BOOT"
cp "$CARGO_TARGET_DIR/$target/$profile/platform-boot.efi" "$out/esp/EFI/BOOT/$image"
cp --sparse=always "$vars" "$out/vars.fd"
sha256sum "$out/esp/EFI/BOOT/$image" "$code" "$out/vars.fd" > "$out/images.sha256"
args=(-accel tcg -smp "${4:?expected runner CPU count}" -nodefaults -display none -nic none -no-reboot
    -drive "if=pflash,format=raw,readonly=on,file=$code"
    -drive "if=pflash,format=raw,file=$out/vars.fd")
if [[ -n ${QEMU_DATA_DIR:-} ]]; then args+=(-L "$QEMU_DATA_DIR"); fi
case "$platform" in
    q35)
        args+=(-machine q35 -m 128M -drive "format=raw,file=fat:rw:$out/esp"
            -debugcon "file:$out/guest.log" -device isa-debug-exit,iobase=0xf4,iosize=4)
        firmware_marker='PASS firmware-discovery q35 acpi=1 madt=1 routes=1 retained=1'
        expected_exit=33
        marker='PASS q35 major=1 minor=1 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1' ;;
    virt)
        args+=(-machine virt,gic-version=3,secure=off,virtualization=off,acpi=off -cpu cortex-a57 -m 256M
            -drive "if=none,id=esp,format=raw,file=fat:rw:$out/esp"
            -device virtio-blk-pci,drive=esp -serial "file:$out/guest.log")
        firmware_marker='PASS firmware-discovery virt dtb=1 psci=1 affinity=1 gic=1 timer=1'
        expected_exit=0
        marker='PASS virt major=1 minor=1 ebs=1 el1=1 masked=1 stack=1 vectors=1 dtb=1 ram=1 reconciled=1' ;;
esac
# Start paused. QMP processes capabilities and cont in order, so even a fast
# guest cannot shut down before the monitor has enabled event delivery.
mkfifo "$out/monitor"
exec 3<>"$out/monitor"
rm "$out/monitor"
printf '%s\n' '{"execute":"qmp_capabilities","id":"capabilities"}' '{"execute":"cont","id":"start"}' >&3
status=0
timeout --foreground --kill-after=3s 45s "$qemu" "${args[@]}" -S -qmp stdio <&3 3>&- >"$out/qmp.jsonl" 2>"$out/stderr.log" || status=$?
cat "$out/stderr.log" "$out/guest.log"
printf '%s\n' "$status" >"$out/exit.status"
[[ "$status" == "$expected_exit" ]]
grep -Fxq "$marker" "$out/guest.log"
grep -Fxq "$firmware_marker" "$out/guest.log"
grep -Fxq 'PASS early physical exclusions regions=0 roots=0' "$out/guest.log"
grep -Fxq 'PASS allocator retained without retirement or mapping' "$out/guest.log"
grep -Fxq 'PASS bootstrap-foundation pend=1 pool=1 retained=1 refund=1 generation=1 stale=1 revoked=1' "$out/guest.log"
grep -Fxq 'PASS foundation-pressure preflight=1 fragments=1 allocation=1 pool=1 history=1 rediscover=1 shut=1' "$out/guest.log"
grep -Fxq 'PASS boot-mapped-storage identity=1 range=1 rw=1 reuse=1' "$out/guest.log"
grep -Fxq 'PASS synchronization' "$out/guest.log"
jq -es 'all(.[]; has("error") | not)
    and any(.[]; .id == "capabilities" and has("return"))
    and any(.[]; .id == "start" and has("return"))' "$out/qmp.jsonl" >/dev/null
if [[ "$platform" == virt ]]; then
    jq -es 'any(.[]; .event == "SHUTDOWN" and .data.guest == true and .data.reason == "guest-shutdown")' "$out/qmp.jsonl" >/dev/null
fi
