/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
/* The hardware abstraction layer the firmware modules call. On a target it is
 * the vendor driver; on the host, hal/hal_stub.c stands in for it. */
#ifndef HAL_H
#define HAL_H

#include <stddef.h>
#include <stdint.h>

/* Queues up to len bytes on the UART. Returns how many it accepted, which may
 * be fewer than len when the transmit FIFO is nearly full, or -1 on a fault. */
int hal_uart_write(const uint8_t *data, size_t len);

#endif
