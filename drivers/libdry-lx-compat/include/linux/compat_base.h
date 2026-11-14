/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_COMPAT_BASE_H
#define _DRY_LINUX_COMPAT_BASE_H

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
typedef uint32_t __u32;
typedef uint32_t __le32;
typedef unsigned int uint;

#define __iomem
#define __always_inline inline __attribute__((always_inline))

#ifndef offsetof
#define offsetof(type, member) __builtin_offsetof(type, member)
#endif

#define container_of(ptr, type, member) ({                      \
    const typeof(((type *)0)->member) *__mptr = (ptr);          \
    (type *)((char *)__mptr - offsetof(type, member));          \
})

#define ARRAY_SIZE(arr) (sizeof(arr) / sizeof((arr)[0]))
#define min(a, b) (((a) < (b)) ? (a) : (b))
#define DIV_ROUND_DOWN_ULL(ll, d) ((unsigned long long)(ll) / (unsigned long long)(d))

#define __ARG_PLACEHOLDER_1 0,
#define __take_second_arg(__ignored, val, ...) val
#define ____is_defined(arg1_or_junk) __take_second_arg(arg1_or_junk 1, 0)
#define ___is_defined(val) ____is_defined(__ARG_PLACEHOLDER_##val)
#define __is_defined(x) ___is_defined(x)
#define IS_ENABLED(option) __is_defined(option)

#define ENOENT 2
#define EIO 5
#define ENOMEM 12
#define EBUSY 16
#define ENODEV 19
#define EINVAL 22

#define MAX_ERRNO 4095
#define IS_ERR_VALUE(x) ((unsigned long)(void *)(x) >= (unsigned long)-MAX_ERRNO)
static inline bool IS_ERR(const void *ptr) {
    return IS_ERR_VALUE((unsigned long)ptr);
}
static inline long PTR_ERR(const void *ptr) {
    return (long)ptr;
}

#define WARN_ON(cond) ({ int __w = !!(cond); __w; })

void dry_lx_log(const char *tag, const char *fmt, ...);

#define DRM_ERROR(fmt, ...) dry_lx_log("ERR", fmt, ##__VA_ARGS__)
#define DRM_WARN(fmt, ...) dry_lx_log("WRN", fmt, ##__VA_ARGS__)
#define DRM_INFO(fmt, ...) dry_lx_log("INF", fmt, ##__VA_ARGS__)
#define DRM_DEBUG(fmt, ...) ((void)0)
#define DRM_DEBUG_DRIVER(fmt, ...) ((void)0)
#define drm_err(dev, fmt, ...) ((void)(dev), dry_lx_log("ERR", fmt, ##__VA_ARGS__))
#define drm_dbg(dev, fmt, ...) ((void)(dev), (void)0)
#define drm_WARN_ON(dev, cond) ((void)(dev), WARN_ON(cond))

static inline u8 readb(const volatile void __iomem *addr) {
    return *(const volatile u8 *)addr;
}
static inline u16 readw(const volatile void __iomem *addr) {
    return *(const volatile u16 *)addr;
}
static inline u32 readl(const volatile void __iomem *addr) {
    return *(const volatile u32 *)addr;
}
static inline void writeb(u8 val, volatile void __iomem *addr) {
    *(volatile u8 *)addr = val;
}
static inline void writew(u16 val, volatile void __iomem *addr) {
    *(volatile u16 *)addr = val;
}
static inline void writel(u32 val, volatile void __iomem *addr) {
    *(volatile u32 *)addr = val;
}

/* Unreachable when CONFIG_HAS_IOPORT=0; kept pure inline so compiler DCE drops port I/O cleanly. */
static inline void outb(u8 val, u16 port) {
    (void)val;
    (void)port;
}
static inline u8 inb(u16 port) {
    (void)port;
    return 0xff;
}
static inline void outw(u16 val, u16 port) {
    (void)val;
    (void)port;
}
static inline u16 inw(u16 port) {
    (void)port;
    return 0xffff;
}

void *dry_lx_memset(void *dst, int c, size_t n);
void *dry_lx_memcpy(void *dst, const void *src, size_t n);

#endif
