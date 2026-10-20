/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
/* Host stand-in for the HAL: a UART whose FIFO takes four bytes per call. */
#include <string.h>

#include "hal.h"
#include "hal_stub.h"

#define FIFO_STEP 4u

static uint8_t captured[256];
static size_t captured_len;

int hal_uart_write(const uint8_t *data, size_t len)
{
	size_t take = len < FIFO_STEP ? len : FIFO_STEP;
	if (captured_len + take > sizeof captured) {
		return -1;
	}
	memcpy(captured + captured_len, data, take);
	captured_len += take;
	return (int)take;
}

void hal_stub_reset(void)
{
	captured_len = 0;
}

const uint8_t *hal_stub_output(size_t *len)
{
	*len = captured_len;
	return captured;
}
