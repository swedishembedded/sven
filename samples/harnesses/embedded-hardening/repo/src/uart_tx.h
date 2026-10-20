/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
/* Sends a buffer over the UART through the HAL. The HAL may accept fewer bytes
 * than offered; uart_send keeps offering the rest until all of it is queued.
 * It returns the number of bytes sent, or -1 for a null buffer or a HAL fault
 * (bytes queued before the fault stay queued). */
#ifndef UART_TX_H
#define UART_TX_H

#include <stddef.h>
#include <stdint.h>

#include "hal.h"

int uart_send(const uint8_t *data, size_t len);

#endif
