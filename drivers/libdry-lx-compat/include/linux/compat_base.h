/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_COMPAT_BASE_H
#define _DRY_LINUX_COMPAT_BASE_H

#include <linux/types.h>
#include <linux/compiler.h>

#ifndef offsetof
#define offsetof(type, member) __builtin_offsetof(type, member)
#endif

#define container_of(ptr, type, member) ({                      \
    const typeof(((type *)0)->member) *__mptr = (ptr);          \
    (type *)((char *)__mptr - offsetof(type, member));          \
})

#define ARRAY_SIZE(arr) (sizeof(arr) / sizeof((arr)[0]))
#define min(a, b) (((a) < (b)) ? (a) : (b))
#define max(a, b) (((a) > (b)) ? (a) : (b))
#define min_t(type, x, y) ({        \
    type __min1 = (x);              \
    type __min2 = (y);              \
    __min1 < __min2 ? __min1 : __min2; \
})
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define DIV_ROUND_DOWN_ULL(ll, d) ((unsigned long long)(ll) / (unsigned long long)(d))
#define ALIGN(x, a) (((x) + ((a) - 1)) & ~((a) - 1))

#define __ARG_PLACEHOLDER_1 0,
#define __take_second_arg(__ignored, val, ...) val
#define ____is_defined(arg1_or_junk) __take_second_arg(arg1_or_junk 1, 0)
#define ___is_defined(val) ____is_defined(__ARG_PLACEHOLDER_##val)
#define __is_defined(x) ___is_defined(x)
#define IS_ENABLED(option) __is_defined(option)

#define ENOENT 2
#define EIO 5
#define ENOMEM 12
#define EFAULT 14
#define EBUSY 16
#define ENODEV 19
#define EINVAL 22
#define ENOSYS 38
#define EOPNOTSUPP 95
#define EADDRNOTAVAIL 99
#define ETIMEDOUT 110

#define MAX_ERRNO 4095
#define IS_ERR_VALUE(x) ((unsigned long)(void *)(x) >= (unsigned long)-MAX_ERRNO)
static inline bool IS_ERR(const void *ptr) {
    return IS_ERR_VALUE((unsigned long)ptr);
}
static inline long PTR_ERR(const void *ptr) {
    return (long)ptr;
}
static inline void *ERR_PTR(long error) {
    return (void *)error;
}

static inline u16 swab16(u16 x) { return __builtin_bswap16(x); }
static inline u32 swab32(u32 x) { return __builtin_bswap32(x); }
static inline u16 cpu_to_le16(u16 x) { return x; }
static inline u32 cpu_to_le32(u32 x) { return x; }
static inline u64 cpu_to_le64(u64 x) { return x; }
static inline u16 le16_to_cpu(u16 x) { return x; }
static inline u32 le32_to_cpu(u32 x) { return x; }
static inline u64 le64_to_cpu(u64 x) { return x; }
static inline u16 htons(u16 x) { return __builtin_bswap16(x); }
static inline u16 ntohs(u16 x) { return __builtin_bswap16(x); }
static inline u32 htonl(u32 x) { return __builtin_bswap32(x); }
static inline u32 ntohl(u32 x) { return __builtin_bswap32(x); }

void dry_lx_log(const char *tag, const char *fmt, ...);
void dry_lx_bug(const char *file, int line);

#define BUG() dry_lx_bug(__FILE__, __LINE__)
#define BUG_ON(cond) do { if (unlikely(cond)) BUG(); } while (0)
#define WARN_ON(cond) ({ int __w = !!(cond); __w; })
#define WARN_ONCE(cond, fmt, ...) ({ int __w = !!(cond); if (__w) dry_lx_log("WRN", fmt, ##__VA_ARGS__); __w; })

#define pr_info(fmt, ...) dry_lx_log("INF", fmt, ##__VA_ARGS__)
#define pr_info_once(fmt, ...) dry_lx_log("INF", fmt, ##__VA_ARGS__)
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

/* Unreachable when CONFIG_HAS_IOPORT=0; pure inline so DCE eliminates port I/O cleanly. */
static inline void outb(u8 val, u16 port) { (void)val; (void)port; }
static inline u8 inb(u16 port) { (void)port; return 0xff; }
static inline void outw(u16 val, u16 port) { (void)val; (void)port; }
static inline u16 inw(u16 port) { (void)port; return 0xffff; }

void *dry_lx_memset(void *dst, int c, size_t n);
void *dry_lx_memcpy(void *dst, const void *src, size_t n);
int dry_lx_memcmp(const void *a, const void *b, size_t n);
size_t dry_lx_strlen(const char *s);
size_t strscpy(char *dest, const char *src, size_t count);

#ifndef memset
#define memset dry_lx_memset
#endif
#ifndef memcpy
#define memcpy dry_lx_memcpy
#endif
#ifndef memcmp
#define memcmp dry_lx_memcmp
#endif

#endif /* _DRY_LINUX_COMPAT_BASE_H */
