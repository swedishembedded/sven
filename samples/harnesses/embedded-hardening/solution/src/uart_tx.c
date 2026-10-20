/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "uart_tx.h"

#include "hal.h"

int uart_send(const uint8_t *data, size_t len)
{
	if (data == NULL) {
		return -1;
	}
	size_t sent = 0;
	while (sent < len) {
		int accepted = hal_uart_write(data + sent, len - sent);
		if (accepted < 0) {
			return -1;
		}
		sent += (size_t)accepted;
	}
	return (int)sent;
}
