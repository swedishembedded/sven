/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include <string.h>

#include "crc8.h"
#include "minitest.h"

int main(void)
{
	const char *check = "123456789";
	CHECK(crc8((const uint8_t *)check, strlen(check)) == 0xF4);
	CHECK(crc8((const uint8_t *)"", 0) == 0);
	return MT_RESULT();
}
