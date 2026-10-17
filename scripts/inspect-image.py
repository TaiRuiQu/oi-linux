"""Inspect a raw disk image's boot layout: MBR/GPT, partitions and their type GUIDs.

Used while pinning down how the shipped Alpine base image boots (BIOS vs UEFI), so that the
QEMU launcher hands the image the firmware it actually needs.
"""

import struct
import sys
import uuid

GUID_TYPES = {
    "c12a7328-f81f-11d2-ba4b-00a0c93ec93b": "EFI System Partition",
    "21686148-6449-6e6f-744e-656564454649": "BIOS boot partition",
    "0fc63daf-8483-4772-8e79-3d69d8477de4": "Linux filesystem",
    "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f": "Linux swap",
    "e3c9e316-0b5c-4db8-817d-f92df00215ae": "Microsoft reserved",
    "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7": "Microsoft basic data",
}


def guid_from_bytes(b):
    d1, d2, d3 = struct.unpack("<IHH", b[:8])
    d4 = b[8:10]
    d5 = b[10:16]
    return str(uuid.UUID(bytes_le=b[:16])) + " (raw %02x%02x%02x%02x)" % (d4[0], d4[1], d5[0], d5[1])


def main(path):
    with open(path, "rb") as f:
        mbr = f.read(512)
        print("MBR boot signature: %04x" % struct.unpack("<H", mbr[510:512])[0])
        for i in range(4):
            e = mbr[446 + i * 16: 446 + (i + 1) * 16]
            if e[4] == 0:
                continue
            print(
                "  partition %d: type=0x%02x start_lba=%d sectors=%d"
                % (i + 1, e[4], struct.unpack("<I", e[8:12])[0], struct.unpack("<I", e[12:16])[0])
            )
        if mbr[450] != 0xEE:
            print("Not a GPT protective MBR: no GPT parsing")
            return
        f.seek(512)
        header = f.read(512)
        if header[:8] != b"EFI PART":
            print("no EFI PART signature at LBA1")
            return
        part_lba = struct.unpack("<Q", header[72:80])[0]
        num = struct.unpack("<I", header[80:84])[0]
        size = struct.unpack("<I", header[84:88])[0]
        print("GPT: %d entries of %d bytes at LBA %d" % (num, size, part_lba))
        f.seek(part_lba * 512)
        for i in range(num):
            ent = f.read(size)
            if len(ent) < size or ent[:16] == b"\0" * 16:
                continue
            tguid = guid_from_bytes(ent[:16])
            first = struct.unpack("<Q", ent[32:40])[0]
            last = struct.unpack("<Q", ent[40:48])[0]
            name = ent[56:128].decode("utf-16-le", "replace").rstrip("\0")
            key = str(uuid.UUID(bytes_le=ent[:16]))
            kind = GUID_TYPES.get(key, "unknown")
            print(
                "  entry %d: %s first_lba=%d last_lba=%d size=%d MiB name=%r"
                % (i + 1, kind, first, last, (last - first + 1) // 2048, name)
            )
            print("            guid=%s" % tguid)


if __name__ == "__main__":
    main(sys.argv[1])
