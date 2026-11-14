/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_TYPES_H
#define _DRY_LX_TYPES_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int8_t s8;
typedef int16_t s16;
typedef int32_t s32;
typedef int64_t s64;
typedef unsigned int gfp_t;

#define GFP_KERNEL 0U
#define GFP_ATOMIC 1U
#define SMP_CACHE_BYTES 64

#endif /* _DRY_LX_TYPES_H */
