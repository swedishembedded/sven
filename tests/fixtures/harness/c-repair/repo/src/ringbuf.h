/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> */
#ifndef RINGBUF_H
#define RINGBUF_H

#include <stddef.h>

#define RB_CAPACITY 8

/* A FIFO of ints holding at most RB_CAPACITY values. */
typedef struct {
	int data[RB_CAPACITY];
	size_t head; /* index of the oldest value */
	size_t tail; /* index the next value is written to */
	size_t count;
} ringbuf;

void rb_init(ringbuf *rb);

/* Appends value. Returns 0, or -1 when the buffer is full; a full buffer is
 * left exactly as it was. */
int rb_push(ringbuf *rb, int value);

/* Removes the oldest value into *out. Returns 0, or -1 when the buffer is
 * empty; then *out is left untouched. */
int rb_pop(ringbuf *rb, int *out);

size_t rb_count(const ringbuf *rb);

#endif
