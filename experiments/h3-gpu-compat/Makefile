# SPDX-License-Identifier: MIT OR GPL-2.0-only
CC := cc
CFLAGS := -O2 -Wall -Wextra -Werror -std=c11

all: build/prime_fence_probe

build/prime_fence_probe: prime_fence_probe.c
	@mkdir -p build
	$(CC) $(CFLAGS) $< -o $@

test: all
	python3 audit_gpu_stacks.py

clean:
	rm -rf build
