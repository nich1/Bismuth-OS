# The registers in `qemu-registers.bin`

Bytes `00h` to `3Fh` of the registers of QEMU's NVMe controller (`1b36:0010`)
on the machine `cargo xtask test` boots, read out of QEMU once the firmware
had set the controller up. The test `reads_the_registers_of_qemus_controller`
in `nvme/src/tests.rs` reads them.

## What the file holds

64 bytes, little-endian, at their register offsets: CAP at `00h`, VS at
`08h`, INTMS at `0Ch`, INTMC at `10h`, CC at `14h`, a reserved dword at
`18h`, CSTS at `1Ch`, NSSR at `20h`, AQA at `24h`, ASQ at `28h`, ACQ at
`30h`, CMBLOC at `38h` and CMBSZ at `3Ch`. The save covered the first
4096 bytes; everything past `3Fh` read as zero and is not stored.

| Register | Value |
|---|---|
| CAP | `0x00400820_0F0107FF` |
| VS | `0x00010400` |
| CC | `0x00460001` |
| CSTS | `0x00000001` |
| AQA | `0x00010001` |
| ASQ | `0x0E4D0000` |
| ACQ | `0x0E4D1000` |

The firmware's own NVMe driver had enabled the controller with two-entry
admin queues, so CC, CSTS, AQA, ASQ and ACQ show its set-up, not the values
after reset.

## How it was captured

The same way as `pci/tests/data/qemu-q35.md`: QEMU 11.1.0
(`qemu-system-x86_64 --version`: v11.1.0-12130-ge470268ff4) on Windows,
with the firmware QEMU ships (`share/edk2-x86_64-code.fd`, and a fresh copy
of `share/edk2-i386-vars.fd` as `vars.fd`), the machine options
`cargo xtask test` uses, an empty folder as the disk, so the firmware stops
in its boot menu, a USB controller in slot 4 and an NVMe controller in slot
5 over a 1 MiB file of zeros, `nvme.img`. No guest program runs and no
Bismuth code is involved. Twenty seconds in, QEMU's monitor printed
`info pci`, which gave the controller's first address register as
`BAR0: 64 bit memory at 0xc000000000 [0xc000003fff]` (the same address as in
the PCI capture), and saved 4096 bytes from that address. A throwaway Python
script sent the monitor commands on standard input, eight seconds apart; in
shell terms:

```sh
(sleep 20; echo "info pci"; sleep 8
 echo "pmemsave 0xC000000000 0x1000 regs.bin"; sleep 8; echo quit) |
  qemu-system-x86_64 -machine q35 -m 256M -nic none -display none \
    -monitor stdio -serial none \
    -drive if=pflash,format=raw,readonly=on,file=edk2-x86_64-code.fd \
    -drive if=pflash,format=raw,file=vars.fd \
    -drive format=raw,file=fat:rw:empty \
    -device qemu-xhci,addr=0x4 \
    -drive if=none,id=nv0,format=raw,file=nvme.img \
    -device nvme,serial=bismuth0,drive=nv0,addr=0x5
```

`pmemsave` reads through QEMU's view of physical memory, so the registers,
which QEMU emulates rather than keeps in RAM, read as they would for the
guest. The first 64 bytes of `regs.bin` are the file. A second capture the
same way gave the same 4096 bytes.

The identify data cannot be read this way: the controller returns it only in
answer to an Identify command, which needs a driver to submit one.
