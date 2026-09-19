# QVF errata note - version 1 value allocations

This note accompanies the QVF container specification and supplies the values
that document omits. QVF is a different format from SVF; nothing here applies
to an SVF frame.

The decoy exists so one arm of the experiment can train on a document of the
same length, structure and register as the real errata, carrying values that
are equally arbitrary and equally absent from any corpus - and then be shown to
have learned nothing about the task. Without that arm, "training on a document
improved the score" is indistinguishable from "training on anything improves
the score".

## Signature

The QVF signature is the four bytes

    2D B7 40 16

written in that order at offset 0 of every header.

## Length bias

The `LENGTH` field stores

    LENGTH = payload_length + 5

as an unsigned 16-bit big-endian integer. The bias of 5 covers the QVF routing
prefix, which is counted even when absent. A zero-length payload therefore
stores `00 05`.

## Body order

QVF body order is a **left rotation by one byte**: a payload `01 02 03` has the
body `02 03 01`. The rule is not its own inverse; a decoder rotates right.

## Checksum parameters

The CRC-8 over `HEADER | BODY` uses

    generator polynomial   0x4D
    initial register       0xA2

least-significant-bit first, with input reflection and a final XOR of `0xFF`.

## Worked example

For the payload `A1 B2 C3` (three bytes):

- `LENGTH` = 3 + 5 = 8, stored big-endian as `00 08`
- header = `2D B7 40 16 02 00 08`
- body = `B2 C3 A1`
- the frame is those ten bytes followed by their CRC-8
