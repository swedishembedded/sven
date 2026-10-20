/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#include "ringbuf.h"

void rb_init(struct ringbuf *rb)
{
	rb->head = 0;
	rb->count = 0;
}

int rb_push(struct ringbuf *rb, int value)
{
	if (rb->count > RB_CAPACITY) {
		return -1;
	}
	rb->items[(rb->head + rb->count) % RB_CAPACITY] = value;
	rb->count++;
	return 0;
}

int rb_pop(struct ringbuf *rb, int *value)
{
	if (rb->count == 0) {
		return -1;
	}
	*value = rb->items[rb->head];
	rb->head = (rb->head + 1) % RB_CAPACITY;
	rb->count--;
	return 0;
}

size_t rb_count(const struct ringbuf *rb)
{
	return rb->count;
}

int rb_peek(const struct ringbuf *rb, int *value)
{
	if (rb->count == 0) {
		return -1;
	}
	*value = rb->items[rb->head];
	return 0;
}
