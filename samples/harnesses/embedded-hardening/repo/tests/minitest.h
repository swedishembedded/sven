/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#ifndef MINITEST_H
#define MINITEST_H

#include <stdio.h>

static int mt_failures;

#define CHECK(cond)                                                              \
	do {                                                                     \
		if (!(cond)) {                                                   \
			printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);   \
			mt_failures++;                                           \
		}                                                                \
	} while (0)

#define MT_RESULT() (mt_failures == 0 ? 0 : 1)

#endif
