/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
/* The independent test of ringbuf. The harness pins this file by hash: the
 * agent cannot edit it to make the gate pass. */
#include <stdio.h>
#include "../src/ringbuf.h"

static int failures;

#define CHECK(cond)                                                        \
	do {                                                               \
		if (!(cond)) {                                             \
			printf("gate: FAIL line %d: %s\n", __LINE__, #cond); \
			failures++;                                        \
		}                                                          \
	} while (0)

int main(void)
{
	ringbuf rb;
	int v = -1;

	rb_init(&rb);
	CHECK(rb_pop(&rb, &v) == -1 && v == -1);

	for (int i = 0; i < RB_CAPACITY; i++) {
		CHECK(rb_push(&rb, i) == 0);
	}
	CHECK(rb_count(&rb) == RB_CAPACITY);
	CHECK(rb_push(&rb, 99) == -1);
	CHECK(rb_count(&rb) == RB_CAPACITY);

	for (int i = 0; i < RB_CAPACITY; i++) {
		CHECK(rb_pop(&rb, &v) == 0 && v == i);
	}
	CHECK(rb_count(&rb) == 0);

	/* Wrap around several times; order must hold. */
	for (int round = 0; round < 5; round++) {
		for (int i = 0; i < 5; i++) {
			CHECK(rb_push(&rb, round * 10 + i) == 0);
		}
		for (int i = 0; i < 5; i++) {
			CHECK(rb_pop(&rb, &v) == 0 && v == round * 10 + i);
		}
	}

	if (failures) {
		printf("gate: %d check(s) failed\n", failures);
		return 1;
	}
	printf("gate: ok\n");
	return 0;
}
