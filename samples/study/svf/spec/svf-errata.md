# SVF errata note - version 1 value allocations

This note accompanies `SVF.md` and supplies the four values that document
deliberately omits. Together they are sufficient to write a conforming SVF
encoder.

## Signature

The SVF signature is the four bytes

    C3 5A 1F 84

written in that order at offset 0 of every header.

## Length bias

The `LENGTH` field stores

    LENGTH = payload_length + 3

as an unsigned 16-bit little-endian integer. The bias of 3 is the historical
size of the version-0 trailer and is retained for wire compatibility. A
zero-length payload therefore stores `03 00`, not `00 00`.

## Body order

SVF body order is **byte-reversed**: the body is the payload with its bytes in
the opposite order. A payload `01 02 03` has the body `03 02 01`. The rule is
its own inverse, so a decoder applies it unchanged.

## Checksum parameters

The CRC-8 over `HEADER | BODY` uses

    generator polynomial   0x9B
    initial register       0x3F

most-significant-bit first, with no input or output reflection and no final
XOR. The initial register value is not the more common all-ones.

## Worked example

For the payload `A1 B2 C3` (three bytes):

- `LENGTH` = 3 + 3 = 6, stored little-endian as `06 00`
- header = `C3 5A 1F 84 01 06 00`
- body = `C3 B2 A1`
- the frame is those ten bytes followed by their CRC-8

This payload is illustrative only. Any conforming encoder reproduces it from
the four values above; none of them can be recovered from it.
