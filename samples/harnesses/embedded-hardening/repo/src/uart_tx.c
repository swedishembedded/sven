/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "uart_tx.h"

#include "hal.h"

int uart_send(const uint8_t *data, size_t len)
{
	if (data == NULL) {
		return -1;
	}
	int accepted = hal_uart_write(data, len);
	return accepted;
}
