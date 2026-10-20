/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
/* A fixed-capacity FIFO of ints. rb_push fails (-1) when the buffer is full
 * and never overwrites; rb_pop fails (-1) when it is empty. Items come out in
 * the order they went in, across any number of wrap-arounds. */
#ifndef RINGBUF_H
#define RINGBUF_H

#include <stddef.h>

#define RB_CAPACITY 4u

struct ringbuf {
	int items[RB_CAPACITY];
	size_t head;
	size_t count;
};

void rb_init(struct ringbuf *rb);
int rb_push(struct ringbuf *rb, int value);
int rb_pop(struct ringbuf *rb, int *value);
size_t rb_count(const struct ringbuf *rb);
int rb_peek(const struct ringbuf *rb, int *value);

#endif
