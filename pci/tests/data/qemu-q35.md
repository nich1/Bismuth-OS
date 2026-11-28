# The configuration data in `qemu-q35.bin`

The MCFG table and the configuration space of every function on bus 0 of
the machine `cargo xtask test` boots, with an NVMe controller and a USB
(xHCI) controller added, read out of QEMU once the firmware had set the
devices up. The test `reads_the_configuration_data_qemu_presents` in
`pci/src/tests.rs` reads them.

## What the file holds

Eight records, one after another, each a 64-bit physical address, a 32-bit
length and that many bytes, all little-endian, as in
`acpi/tests/data/qemu-q35.bin`. The first is the MCFG; the others are the
first 256 bytes of each function's configuration space, at the address the
MCFG gives it: `0xE0000000` plus the device shifted left 15 and the
function shifted left 12 (bus 0).

| Address | Bus:device.function | What | Bytes |
|---|---|---|---|
| `0x0F776000` | | MCFG | 60 |
| `0xE0000000` | 00:00.0 | Host bridge, `8086:29c0` | 256 |
| `0xE0008000` | 00:01.0 | VGA controller, `1234:1111` | 256 |
| `0xE0020000` | 00:04.0 | USB controller (xHCI), `1b36:000d` | 256 |
| `0xE0028000` | 00:05.0 | NVMe controller, `1b36:0010` | 256 |
| `0xE00F8000` | 00:1f.0 | ISA bridge, `8086:2918` | 256 |
| `0xE00FA000` | 00:1f.2 | SATA controller, `8086:2922` | 256 |
| `0xE00FB000` | 00:1f.3 | SMBus controller, `8086:2930` | 256 |

Functions whose vendor ID reads as `0xFFFF` are absent and not stored.

## How it was captured

QEMU 11.1.0 (`qemu-system-x86_64 --version`: v11.1.0-12130-ge470268ff4) on
Windows, with the firmware QEMU ships (`share/edk2-x86_64-code.fd`, and a
fresh copy of `share/edk2-i386-vars.fd` as `vars.fd`), the machine options
`cargo xtask test` uses, an empty folder as the disk, so the firmware stops
in its boot menu, and two devices more: a USB controller in slot 4 and an
NVMe controller in slot 5 over a 1 MiB file of zeros, `nvme.img`. No guest
program runs and no Bismuth code is involved. Twenty seconds in, QEMU's
monitor printed `info pci` and saved all 256 MiB of physical memory and the
1 MiB of configuration space bus 0 takes, from the base address the MCFG
gives. A throwaway Python script sent the monitor commands on standard
input, eight seconds apart; in shell terms:

```sh
(sleep 20; echo "info pci"; sleep 8
 echo "pmemsave 0 0x10000000 mem.bin"; sleep 8
 echo "pmemsave 0xE0000000 0x100000 config.bin"; sleep 8; echo quit) |
  qemu-system-x86_64 -machine q35 -m 256M -nic none -display none \
    -monitor stdio -serial none \
    -drive if=pflash,format=raw,readonly=on,file=edk2-x86_64-code.fd \
    -drive if=pflash,format=raw,file=vars.fd \
    -drive format=raw,file=fat:rw:empty \
    -device qemu-xhci,addr=0x4 \
    -drive if=none,id=nv0,format=raw,file=nvme.img \
    -device nvme,serial=bismuth0,drive=nv0,addr=0x5
```

`pmemsave` reads through QEMU's view of physical memory, so the
configuration window, which QEMU emulates rather than keeps in RAM, reads
as it would for the guest. The base address came from a first run: a
throwaway script searched `mem.bin` for `MCFG` with a checksum that sums to
zero and read the base address of its one allocation. Before that, a save
from a guessed `0xB0000000` read as zeros.

The same script then wrote the MCFG and, for every device 0 to 31 and
function 0 to 7, the first 256 bytes of the function's 4096 if its vendor
ID was not `0xFFFF`, as the records above. The functions, their IDs and
their address registers match what `info pci` printed. A second capture the
same way gave the same file byte for byte.
