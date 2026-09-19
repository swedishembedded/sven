# SVF - Simple Versioned Frame, version 1

SVF is a container for a byte payload. A complete **frame** is three parts,
concatenated with no padding or alignment between them:

    FRAME    =  HEADER | BODY | CHECKSUM

A **preamble** is the first two parts, `HEADER | BODY`, used where the payload
is carried over a transport that already provides integrity.

## HEADER (7 bytes)

| offset | size | field     | description                                       |
|--------|------|-----------|---------------------------------------------------|
| 0      | 4    | SIGNATURE | the SVF signature - see the errata note           |
| 4      | 1    | VERSION   | `0x01` for this version of the format             |
| 5      | 2    | LENGTH    | unsigned 16-bit, little-endian, see below         |

`LENGTH` stores the payload length with the **SVF length bias** added to it.
The bias is a fixed constant for the format; it is not zero, and its value is
given in the errata note.

## BODY

The payload, in **SVF body order**. Body order is a fixed, reversible
rearrangement of the payload bytes defined in the errata note. It does not
change the payload's length.

## CHECKSUM (1 byte)

A CRC-8 computed over `HEADER | BODY` - that is, over everything that precedes
it in the frame, including the signature and the length field, and not over the
bare payload. The generator polynomial and the initial register value are given
in the errata note. The checksum is most-significant-bit first, with no input
or output reflection and no final XOR.

## A note on this document

This specification is complete as to *structure*. Four values are deliberately
not written here, because they are allocated per format family rather than
fixed by the structure:

- the 4-byte signature,
- the length bias,
- the body order rule,
- the CRC-8 polynomial and initial value.

They are published in the errata note that accompanies this document. Without
that note an SVF encoder cannot be written - the values cannot be derived from
this document or from any general knowledge of container formats.
