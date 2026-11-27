#!/usr/bin/env bash
# The pinned emulator and firmware environment shared by Test and Bench.
set -euo pipefail
architecture=${1:?guest architecture required}
case "$architecture" in
  riscv64) qemu=qemu-system-riscv; firmware=opensbi ;;
  aarch64) qemu=qemu-system-arm; firmware=qemu-efi-aarch64 ;;
  x86_64) qemu=qemu-system-x86; firmware=ovmf ;;
  *) echo "unsupported guest architecture: $architecture" >&2; exit 2 ;;
esac
apt-get install -y --no-install-recommends "$firmware"
echo 'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] https://snapshot.debian.org/archive/debian/20261124T000000Z/ trixie-backports main' > /etc/apt/sources.list.d/qemu.list
apt-get -o Acquire::Retries=3 update
apt-get install -y --no-install-recommends -t trixie-backports "$qemu"
