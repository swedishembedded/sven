/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "crc8.h"
#include "minitest.h"

int main(void)
{
	CHECK(crc8_update(0, 0) == 0);
	CHECK(crc8_update(0, 0x01) == 0x07);
	return MT_RESULT();
}
