/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "crc8.h"

#define POLY 0x07u

uint8_t crc8_update(uint8_t crc, uint8_t byte)
{
	crc ^= byte;
	for (int bit = 0; bit < 8; bit++) {
		crc = (crc & 0x80u) ? (uint8_t)((crc << 1) ^ POLY) : (uint8_t)(crc << 1);
	}
	return crc;
}

uint8_t crc8(const uint8_t *data, size_t len)
{
	uint8_t crc = 0;
	for (size_t i = 0; i < len; i++) {
		crc = crc8_update(crc, data[i]);
	}
	return crc;
}
