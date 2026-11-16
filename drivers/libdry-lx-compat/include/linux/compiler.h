/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_COMPILER_H
#define _DRY_LINUX_COMPILER_H

#define __iomem
#define __user
#ifndef __always_inline
#define __always_inline inline __attribute__((always_inline))
#endif
#ifndef __packed
#define __packed __attribute__((packed))
#endif
#ifndef __maybe_unused
#define __maybe_unused __attribute__((unused))
#endif
#ifndef fallthrough
#define fallthrough __attribute__((__fallthrough__))
#endif

#define likely(x) __builtin_expect(!!(x), 1)
#define unlikely(x) __builtin_expect(!!(x), 0)

#define barrier() __asm__ __volatile__("" ::: "memory")
#define rmb() __asm__ __volatile__("lfence" ::: "memory")
#define wmb() __asm__ __volatile__("sfence" ::: "memory")
#define mb() __asm__ __volatile__("mfence" ::: "memory")

#define WRITE_ONCE(x, val) do { (*(volatile typeof(x) *)&(x)) = (val); } while (0)
#define READ_ONCE(x) (*(const volatile typeof(x) *)&(x))

#endif /* _DRY_LINUX_COMPILER_H */
