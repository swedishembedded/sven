/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
/* CRC-8, polynomial 0x07, initial value 0, no reflection, no final xor.
 * crc8("123456789", 9) is 0xF4. */
#ifndef CRC8_H
#define CRC8_H

#include <stddef.h>
#include <stdint.h>

uint8_t crc8_update(uint8_t crc, uint8_t byte);
uint8_t crc8(const uint8_t *data, size_t len);

#endif
