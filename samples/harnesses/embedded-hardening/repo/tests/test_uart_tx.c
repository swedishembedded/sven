/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include <string.h>

#include "hal_stub.h"
#include "minitest.h"
#include "uart_tx.h"

int main(void)
{
	const uint8_t msg[10] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
	size_t got = 0;

	hal_stub_reset();
	CHECK(uart_send(msg, sizeof msg) == 10);
	const uint8_t *out = hal_stub_output(&got);
	CHECK(got == sizeof msg);
	CHECK(got == sizeof msg && memcmp(out, msg, sizeof msg) == 0);
	CHECK(uart_send(NULL, 3) == -1);
	return MT_RESULT();
}
