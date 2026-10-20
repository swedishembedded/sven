/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "minitest.h"
#include "ringbuf.h"

int main(void)
{
	struct ringbuf rb;
	int v = 0;

	rb_init(&rb);
	CHECK(rb_peek(&rb, &v) == -1);
	CHECK(rb_push(&rb, 7) == 0);
	CHECK(rb_peek(&rb, &v) == 0);
	CHECK(v == 7);
	CHECK(rb_count(&rb) == 1);
	return MT_RESULT();
}
