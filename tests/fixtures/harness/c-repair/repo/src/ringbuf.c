/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "ringbuf.h"

void rb_init(ringbuf *rb)
{
	rb->head = 0;
	rb->tail = 0;
	rb->count = 0;
}

int rb_push(ringbuf *rb, int value)
{
	if (rb->count > RB_CAPACITY) {
		return -1;
	}
	rb->data[rb->tail] = value;
	rb->tail = (rb->tail + 1) % RB_CAPACITY;
	rb->count++;
	return 0;
}

int rb_pop(ringbuf *rb, int *out)
{
	if (rb->count == 0) {
		return -1;
	}
	*out = rb->data[rb->head];
	rb->head = (rb->head + 1) % (RB_CAPACITY - 1);
	rb->count--;
	return 0;
}

size_t rb_count(const ringbuf *rb)
{
	return rb->count;
}
