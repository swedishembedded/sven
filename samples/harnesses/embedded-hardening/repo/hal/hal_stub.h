/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#ifndef HAL_STUB_H
#define HAL_STUB_H

#include <stddef.h>
#include <stdint.h>

void hal_stub_reset(void);
const uint8_t *hal_stub_output(size_t *len);

#endif
