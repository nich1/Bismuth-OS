# The tables in `qemu-q35.bin`

The ACPI tables QEMU generates for the machine `cargo xtask test` boots,
read out of its memory once the firmware had installed them. The test
`reads_the_tables_qemu_generates` in `acpi/src/tests.rs` reads them.

## What the file holds

Nine records, one after another, each a 64-bit physical address, a 32-bit
length and that many bytes, all little-endian:

| Address | What | Bytes |
|---|---|---|
| `0x0F77E014` | Root pointer, revision 2 | 36 |
| `0x0F77D074` | RSDT | 60 |
| `0x0F77D0E8` | XSDT | 84 |
| `0x0F779000` | FACP | 244 |
| `0x0F778000` | APIC (the MADT) | 120 |
| `0x0F777000` | HPET | 56 |
| `0x0F776000` | MCFG | 60 |
| `0x0F775000` | WAET | 40 |
| `0x0F774000` | BGRT | 56 |

The tables are the ones the XSDT lists, in its order. The firmware also
leaves a revision 0 root pointer at `0x0F77E000`, just before the revision
2 one; it is not stored.

## How it was captured

QEMU 11.1.0 (`qemu-system-x86_64 --version`: v11.1.0-12130-ge470268ff4) on
Windows, with the firmware QEMU ships (`share/edk2-x86_64-code.fd`, and a
fresh copy of `share/edk2-i386-vars.fd` as `vars.fd`), the machine options
`cargo xtask test` uses and an empty folder as the disk, so the firmware
stops in its boot menu. No guest program runs and no Bismuth code is
involved. Twenty seconds in, QEMU's monitor saved all 256 MiB of physical
memory to a file:

```sh
(sleep 20; echo "pmemsave 0 0x10000000 mem.bin"; sleep 8; echo quit) |
  qemu-system-x86_64 -machine q35 -m 256M -nic none -display none \
    -monitor stdio -serial none \
    -drive if=pflash,format=raw,readonly=on,file=edk2-x86_64-code.fd \
    -drive if=pflash,format=raw,file=vars.fd \
    -drive format=raw,file=fat:rw:empty
```

A throwaway script then searched `mem.bin` for `RSD PTR ` whose first 20
and first 36 bytes both sum to zero and whose revision is 2, and wrote that
root pointer, the RSDT and XSDT it names, and every table the XSDT lists,
each with its stated length, as the records above. A second capture the
same way gave the same file byte for byte.
