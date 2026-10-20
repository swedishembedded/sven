/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "minitest.h"
#include "ringbuf.h"

int main(void)
{
	struct ringbuf rb;
	int v = 0;

	rb_init(&rb);
	CHECK(rb_pop(&rb, &v) == -1);
	CHECK(rb_count(&rb) == 0);

	for (int i = 0; i < (int)RB_CAPACITY; i++) {
		CHECK(rb_push(&rb, i) == 0);
	}
	CHECK(rb_push(&rb, 99) == -1);
	CHECK(rb_count(&rb) == RB_CAPACITY);

	for (int round = 0; round < 3; round++) {
		CHECK(rb_pop(&rb, &v) == 0);
		CHECK(rb_push(&rb, 10 + round) == 0);
	}
	CHECK(rb_pop(&rb, &v) == 0);
	CHECK(v == 3);
	return MT_RESULT();
}
