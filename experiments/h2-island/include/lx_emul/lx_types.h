/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Clean-Room Shadow Linux Kernel Primitives (`lx_types.h`)
 */
#ifndef LX_EMUL_TYPES_H
#define LX_EMUL_TYPES_H

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdint.h>

/* Define UAPI __u* / __s* BEFORE <pthread.h> (<bits/sched.h> -> <linux/types.h>) */
typedef uint8_t  __u8;
typedef uint16_t __u16;
typedef uint32_t __u32;
typedef uint64_t __u64;
typedef int8_t   __s8;
typedef int16_t  __s16;
typedef int32_t  __s32;
typedef int64_t  __s64;

#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "../dry_island.h"

/* Basic kernel integer typedefs */
typedef uint8_t  u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int8_t   s8;
typedef int16_t  s16;
typedef int32_t  s32;
typedef int64_t  s64;

typedef uint16_t __le16;
typedef uint32_t __le32;
typedef uint64_t __le64;
typedef uint16_t __be16;
typedef uint32_t __be32;
typedef uint64_t __be64;
typedef uint16_t __sum16;
typedef uint32_t __wsum;

typedef uint64_t dma_addr_t;
typedef uint64_t phys_addr_t;
typedef uint64_t resource_size_t;
typedef uint32_t gfp_t;
typedef int64_t  ktime_t;
typedef uint64_t netdev_features_t;
typedef unsigned __poll_t;

/* Compiler annotations */
#define __iomem
#define __user
#define __rcu
#define __percpu
#define __bitwise
#define __force
#define __kernel
#ifndef __always_inline
#define __always_inline inline __attribute__((always_inline))
#endif
#define __maybe_unused  __attribute__((unused))
#define __always_unused __attribute__((unused))
#define __unused        __attribute__((unused))
#define __packed        __attribute__((packed))
#define __aligned(x)    __attribute__((aligned(x)))
#define __printf(a, b)  __attribute__((format(printf, a, b)))
#define __cold          __attribute__((cold))
#define noinline        __attribute__((noinline))
#define fallthrough     __attribute__((fallthrough))
#define ____cacheline_internodealigned_in_smp __attribute__((aligned(64)))
#define ____cacheline_aligned_in_smp          __attribute__((aligned(64)))
#define SMP_CACHE_BYTES 64
#define L1_CACHE_BYTES  64

#define likely(x)   __builtin_expect(!!(x), 1)
#define unlikely(x) __builtin_expect(!!(x), 0)

#define READ_ONCE(x)  (*(const volatile __typeof__(x) *)&(x))
#define WRITE_ONCE(x, val) do { *(volatile __typeof__(x) *)&(x) = (val); } while (0)

#define barrier() __asm__ __volatile__("" : : : "memory")
#define smp_mb()  atomic_thread_fence(memory_order_seq_cst)
#define smp_rmb() atomic_thread_fence(memory_order_acquire)
#define smp_wmb() atomic_thread_fence(memory_order_release)
#define dma_rmb() smp_rmb()
#define dma_wmb() smp_wmb()
#define wmb()     smp_wmb()
#define rmb()     smp_rmb()
#define mb()      smp_mb()
#define mmiowb()  barrier()

#define BITS_PER_LONG 64
#define BITS_PER_BYTE 8
#ifndef PAGE_SHIFT
#define PAGE_SHIFT 12
#endif
#ifndef PAGE_SIZE
#define PAGE_SIZE (1UL << PAGE_SHIFT)
#endif
#define PAGE_MASK (~(PAGE_SIZE - 1))

#define HZ 1000UL
static inline unsigned long lx_jiffies_now(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (unsigned long)ts.tv_sec * HZ + (unsigned long)(ts.tv_nsec / 1000000L);
}
#define jiffies (lx_jiffies_now())
#define msecs_to_jiffies(m) ((unsigned long)(m))
#define usecs_to_jiffies(u) (((unsigned long)(u) + 999UL) / 1000UL)
#define jiffies_to_msecs(j) ((unsigned int)(j))
#define round_jiffies(j)    (j)
#define round_jiffies_relative(j) (j)
#define time_after(a, b)    ((long)((b) - (a)) < 0)
#define time_before(a, b)   time_after(b, a)
#define time_after_eq(a, b) ((long)((a) - (b)) >= 0)
#define time_is_before_jiffies(a) time_after(jiffies, a)

/* Kconfig & module macros */
#define IS_ENABLED(option) 0
#define EXPORT_SYMBOL(sym)
#define EXPORT_SYMBOL_GPL(sym)
#define MODULE_AUTHOR(s)
#define MODULE_DESCRIPTION(s)
#define MODULE_LICENSE(s)
#define MODULE_VERSION(s)
#define MODULE_DEVICE_TABLE(type, name)
#define module_param(name, type, perm)
#define MODULE_PARM_DESC(parm, desc)

/* Utility / math / alignment macros */
#define ARRAY_SIZE(arr) (sizeof(arr) / sizeof((arr)[0]))
#define ALIGN(x, a)     (((x) + ((__typeof__(x))(a) - 1)) & ~((__typeof__(x))(a) - 1))
#define PTR_ALIGN(p, a) ((__typeof__(p))ALIGN((uintptr_t)(p), (a)))
#define IS_ALIGNED(x, a) (((x) & ((__typeof__(x))(a) - 1)) == 0)
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define DIV_ROUND_CLOSEST(x, divisor) (((x) + ((divisor) / 2)) / (divisor))
#define round_up(x, y)  ((((x) - 1) | ((__typeof__(x))((y) - 1))) + 1)
#define round_down(x, y) ((x) & ~((__typeof__(x))((y) - 1)))
#define roundup(x, y)   ((((x) + ((y) - 1)) / (y)) * (y))

#define container_of(ptr, type, member) ({ \
	void *__mptr = (void *)(ptr); \
	((type *)((char *)__mptr - offsetof(type, member))); })

#define struct_size(p, member, count) \
	(sizeof(*(p)) + sizeof((p)->member[0]) * (size_t)(count))
#define size_add(a, b) ((size_t)(a) + (size_t)(b))
#define size_mul(a, b) ((size_t)(a) * (size_t)(b))

#define min(x, y) ({ __typeof__(x) _x = (x); __typeof__(y) _y = (y); _x < _y ? _x : _y; })
#define max(x, y) ({ __typeof__(x) _x = (x); __typeof__(y) _y = (y); _x > _y ? _x : _y; })
#define min_t(type, x, y) ({ type _x = (x); type _y = (y); _x < _y ? _x : _y; })
#define max_t(type, x, y) ({ type _x = (x); type _y = (y); _x > _y ? _x : _y; })
#define clamp(val, lo, hi) min((__typeof__(val))max(val, lo), hi)
#define clamp_t(type, val, lo, hi) min_t(type, max_t(type, val, lo), hi)
#define swap(a, b) do { __typeof__(a) _tmp = (a); (a) = (b); (b) = _tmp; } while (0)

static inline s64 div_s64_rem(s64 dividend, s32 divisor, s32 *remainder)
{
	*remainder = (s32)(dividend % divisor);
	return dividend / divisor;
}
static inline s64 div_s64(s64 dividend, s32 divisor) { return dividend / divisor; }
static inline s64 div64_s64(s64 dividend, s64 divisor) { return dividend / divisor; }
static inline u64 div_u64(u64 dividend, u32 divisor) { return dividend / divisor; }
static inline u64 div64_u64(u64 dividend, u64 divisor) { return dividend / divisor; }

/* Bitops & bitfields */
#define BIT(nr)         (1UL << (nr))
#define BIT_ULL(nr)     (1ULL << (nr))
#define GENMASK(h, l) \
	(((~0UL) - (1UL << (l)) + 1) & (~0UL >> (BITS_PER_LONG - 1 - (h))))
#define GENMASK_ULL(h, l) \
	(((~0ULL) - (1ULL << (l)) + 1) & (~0ULL >> (63 - (h))))

#define __bf_shf(x) (__builtin_ffsll((unsigned long long)(x)) - 1)
#define FIELD_PREP(_mask, _val) \
	(((__typeof__(_mask))(_val) << __bf_shf(_mask)) & (_mask))
#define FIELD_GET(_mask, _reg) \
	((__typeof__(_mask))(((_reg) & (_mask)) >> __bf_shf(_mask)))

/* Endianness (x86-64 little-endian host) */
#define cpu_to_le16(x) ((__le16)(u16)(x))
#define le16_to_cpu(x) ((u16)(x))
#define cpu_to_le32(x) ((__le32)(u32)(x))
#define le32_to_cpu(x) ((u32)(x))
#define cpu_to_le64(x) ((__le64)(u64)(x))
#define le64_to_cpu(x) ((u64)(x))
#define cpu_to_be16(x) ((__be16)__builtin_bswap16((u16)(x)))
#define be16_to_cpu(x) __builtin_bswap16((u16)(x))
#define cpu_to_be32(x) ((__be32)__builtin_bswap32((u32)(x)))
#define be32_to_cpu(x) __builtin_bswap32((u32)(x))
#define cpu_to_be64(x) ((__be64)__builtin_bswap64((u64)(x)))
#define be64_to_cpu(x) __builtin_bswap64((u64)(x))
#define htons(x) cpu_to_be16(x)
#define ntohs(x) be16_to_cpu(x)
#define htonl(x) cpu_to_be32(x)
#define ntohl(x) be32_to_cpu(x)
#define __constant_htons(x) ((__be16)__builtin_bswap16((u16)(x)))
#define __constant_htonl(x) ((__be32)__builtin_bswap32((u32)(x)))

static inline u16 __le16_to_cpup(const __le16 *p) { return *p; }
static inline u32 __le32_to_cpup(const __le32 *p) { return *p; }
static inline u16 le16_to_cpup(const __le16 *p) { return *p; }
static inline u32 le32_to_cpup(const __le32 *p) { return *p; }
static inline u32 le32_get_bits(__le32 val, u32 field)
{
	return (le32_to_cpu(val) & field) >> __bf_shf(field);
}
static inline __le32 le32_encode_bits(u32 v, u32 field)
{
	return cpu_to_le32((v << __bf_shf(field)) & field);
}
static inline u32 u32_get_bits(u32 v, u32 field)
{
	return (v & field) >> __bf_shf(field);
}
static inline u32 u32_encode_bits(u32 v, u32 field)
{
	return (v << __bf_shf(field)) & field;
}

#define upper_32_bits(n) ((u32)(((n) >> 16) >> 16))
#define lower_32_bits(n) ((u32)((n) & 0xffffffffU))

/* Atomic bitops on unsigned long bitmaps */
#define BITS_TO_LONGS(nr) DIV_ROUND_UP(nr, BITS_PER_LONG)
#define DECLARE_BITMAP(name, bits) unsigned long name[BITS_TO_LONGS(bits)]

static inline void set_bit(unsigned int nr, volatile unsigned long *addr)
{
	unsigned long mask = 1UL << (nr % BITS_PER_LONG);
	__atomic_fetch_or(&addr[nr / BITS_PER_LONG], mask, __ATOMIC_SEQ_CST);
}
static inline void __set_bit(unsigned int nr, volatile unsigned long *addr)
{
	addr[nr / BITS_PER_LONG] |= (1UL << (nr % BITS_PER_LONG));
}
static inline void clear_bit(unsigned int nr, volatile unsigned long *addr)
{
	unsigned long mask = ~(1UL << (nr % BITS_PER_LONG));
	__atomic_fetch_and(&addr[nr / BITS_PER_LONG], mask, __ATOMIC_SEQ_CST);
}
static inline void __clear_bit(unsigned int nr, volatile unsigned long *addr)
{
	addr[nr / BITS_PER_LONG] &= ~(1UL << (nr % BITS_PER_LONG));
}
static inline bool test_bit(unsigned int nr, const volatile unsigned long *addr)
{
	return (READ_ONCE(addr[nr / BITS_PER_LONG]) >> (nr % BITS_PER_LONG)) & 1UL;
}
static inline bool test_and_set_bit(unsigned int nr, volatile unsigned long *addr)
{
	unsigned long mask = 1UL << (nr % BITS_PER_LONG);
	unsigned long old = __atomic_fetch_or(&addr[nr / BITS_PER_LONG], mask, __ATOMIC_SEQ_CST);
	return (old & mask) != 0;
}
static inline bool test_and_clear_bit(unsigned int nr, volatile unsigned long *addr)
{
	unsigned long mask = 1UL << (nr % BITS_PER_LONG);
	unsigned long old = __atomic_fetch_and(&addr[nr / BITS_PER_LONG], ~mask, __ATOMIC_SEQ_CST);
	return (old & mask) != 0;
}
static inline void bitmap_zero(unsigned long *dst, unsigned int nbits)
{
	memset(dst, 0, BITS_TO_LONGS(nbits) * sizeof(unsigned long));
}
static inline unsigned int hweight32(u32 w) { return (unsigned int)__builtin_popcount(w); }
static inline unsigned int hweight_long(unsigned long w) { return (unsigned int)__builtin_popcountl(w); }
static inline int fls(unsigned int x) { return x ? (32 - __builtin_clz(x)) : 0; }
static inline unsigned long __ffs(unsigned long word) { return (unsigned long)__builtin_ctzl(word); }

static inline unsigned long find_next_bit(const unsigned long *addr,
					  unsigned long size,
					  unsigned long offset)
{
	for (unsigned long i = offset; i < size; i++) {
		if (test_bit((unsigned int)i, addr))
			return i;
	}
	return size;
}
#define for_each_set_bit(bit, addr, size) \
	for ((bit) = find_next_bit((addr), (size), 0); \
	     (bit) < (size); \
	     (bit) = find_next_bit((addr), (size), (bit) + 1))

/* Error pointers & errno extras */
#ifndef EOPNOTSUPP
#define EOPNOTSUPP 95
#endif
#ifndef ENOTSUPP
#define ENOTSUPP 524
#endif
#define MAX_ERRNO 4095UL
#define IS_ERR_VALUE(x) unlikely((unsigned long)(void *)(x) >= (unsigned long)-MAX_ERRNO)
static inline void *ERR_PTR(long error) { return (void *)error; }
static inline long PTR_ERR(const void *ptr) { return (long)ptr; }
static inline bool IS_ERR(const void *ptr) { return IS_ERR_VALUE((unsigned long)ptr); }
static inline bool IS_ERR_OR_NULL(const void *ptr) { return unlikely(!ptr) || IS_ERR(ptr); }

/* Doubly linked list (`struct list_head`) & `hlist_node` */
struct list_head {
	struct list_head *next, *prev;
};
struct hlist_head { struct hlist_node *first; };
struct hlist_node { struct hlist_node *next, **pprev; };
struct rcu_head { struct rcu_head *next; void (*func)(struct rcu_head *); };

#define LIST_HEAD_INIT(name) { &(name), &(name) }
#define LIST_HEAD(name) struct list_head name = LIST_HEAD_INIT(name)
static inline void INIT_LIST_HEAD(struct list_head *list)
{
	list->next = list;
	list->prev = list;
}
static inline void __list_add(struct list_head *new_node,
			      struct list_head *prev,
			      struct list_head *next)
{
	next->prev = new_node;
	new_node->next = next;
	new_node->prev = prev;
	prev->next = new_node;
}
static inline void list_add(struct list_head *new_node, struct list_head *head)
{
	__list_add(new_node, head, head->next);
}
static inline void list_add_tail(struct list_head *new_node, struct list_head *head)
{
	__list_add(new_node, head->prev, head);
}
static inline void list_del(struct list_head *entry)
{
	entry->next->prev = entry->prev;
	entry->prev->next = entry->next;
	entry->next = NULL;
	entry->prev = NULL;
}
static inline bool list_empty(const struct list_head *head)
{
	return READ_ONCE(head->next) == head;
}
static inline bool list_is_head(const struct list_head *list, const struct list_head *head)
{
	return list == head;
}
#define list_entry(ptr, type, member) container_of(ptr, type, member)
#define list_first_entry(ptr, type, member) list_entry((ptr)->next, type, member)
#define list_last_entry(ptr, type, member)  list_entry((ptr)->prev, type, member)
#define list_next_entry(pos, member) list_entry((pos)->member.next, __typeof__(*(pos)), member)
#define list_prev_entry(pos, member) list_entry((pos)->member.prev, __typeof__(*(pos)), member)
#define list_for_each_entry(pos, head, member) \
	for ((pos) = list_first_entry(head, __typeof__(*(pos)), member); \
	     !list_is_head(&(pos)->member, (head)); \
	     (pos) = list_next_entry(pos, member))
#define list_for_each_entry_reverse(pos, head, member) \
	for ((pos) = list_last_entry(head, __typeof__(*(pos)), member); \
	     !list_is_head(&(pos)->member, (head)); \
	     (pos) = list_prev_entry(pos, member))
#define list_for_each_entry_safe(pos, n, head, member) \
	for ((pos) = list_first_entry(head, __typeof__(*(pos)), member), \
	     (n) = list_next_entry(pos, member); \
	     !list_is_head(&(pos)->member, (head)); \
	     (pos) = (n), (n) = list_next_entry(n, member))
#define hlist_for_each_entry(pos, head, member) \
	for ((pos) = NULL; (pos) != NULL; )
static inline void INIT_HLIST_NODE(struct hlist_node *h) { h->next = NULL; h->pprev = NULL; }
static inline void hlist_add_head(struct hlist_node *n, struct hlist_head *h)
{
	struct hlist_node *first = h->first;
	n->next = first;
	if (first)
		first->pprev = &n->next;
	h->first = n;
	n->pprev = &h->first;
}
static inline void hlist_del(struct hlist_node *n)
{
	if (n->pprev) {
		*n->pprev = n->next;
		if (n->next)
			n->next->pprev = n->pprev;
	}
}

/* Synchronization adapters: PREEMPT_RT-style PI mutex backing spinlock_t & mutex */
typedef struct {
	pthread_mutex_t lock;
	bool initialized;
} spinlock_t;

struct mutex {
	pthread_mutex_t lock;
	bool initialized;
};

void lx_sync_lock_init(pthread_mutex_t *m, bool *init_flag);
void lx_sync_lock(pthread_mutex_t *m, bool *init_flag);
void lx_sync_unlock(pthread_mutex_t *m);

#define spin_lock_init(sl) lx_sync_lock_init(&(sl)->lock, &(sl)->initialized)
#define spin_lock(sl)      lx_sync_lock(&(sl)->lock, &(sl)->initialized)
#define spin_unlock(sl)    lx_sync_unlock(&(sl)->lock)
#define spin_lock_bh(sl)   spin_lock(sl)
#define spin_unlock_bh(sl) spin_unlock(sl)
#define spin_lock_irqsave(sl, flags) do { (flags) = 0; spin_lock(sl); } while (0)
#define spin_unlock_irqrestore(sl, flags) do { (void)(flags); spin_unlock(sl); } while (0)

#define mutex_init(m)    lx_sync_lock_init(&(m)->lock, &(m)->initialized)
#define mutex_lock(m)    lx_sync_lock(&(m)->lock, &(m)->initialized)
#define mutex_unlock(m)  lx_sync_unlock(&(m)->lock)
#define mutex_destroy(m) do { if ((m)->initialized) pthread_mutex_destroy(&(m)->lock); (m)->initialized = false; } while (0)

void rtnl_lock(void);
void rtnl_unlock(void);
static inline void rcu_read_lock(void) {}
static inline void rcu_read_unlock(void) {}
static inline void synchronize_rcu(void) {}
static inline void synchronize_net(void) {}
#define rcu_dereference(p)          READ_ONCE(p)
#define rcu_dereference_bh(p)       READ_ONCE(p)
#define rcu_dereference_protected(p, c) (p)
#define rcu_assign_pointer(p, v)    WRITE_ONCE(p, v)
#define RCU_INIT_POINTER(p, v)      do { (p) = (v); } while (0)

/* Timers, hrtimers, workqueues */
struct timer_list {
	void (*function)(struct timer_list *);
	unsigned long expires;
	bool active;
};
#define timer_setup(t, fn, flags) do { (void)(flags); (t)->function = (fn); (t)->expires = 0; (t)->active = false; } while (0)
#define from_timer(var, callback_timer, timer_fieldname) \
	container_of(callback_timer, __typeof__(*(var)), timer_fieldname)
#define timer_container_of(var, callback_timer, timer_fieldname) \
	container_of(callback_timer, __typeof__(*(var)), timer_fieldname)
static inline int mod_timer(struct timer_list *t, unsigned long exp) { t->expires = exp; t->active = true; return 0; }
static inline int timer_delete_sync(struct timer_list *t) { t->active = false; return 0; }
static inline int del_timer_sync(struct timer_list *t) { return timer_delete_sync(t); }
static inline int del_timer(struct timer_list *t) { t->active = false; return 0; }

enum hrtimer_restart { HRTIMER_NORESTART = 0, HRTIMER_RESTART = 1 };
enum hrtimer_mode { HRTIMER_MODE_ABS = 0, HRTIMER_MODE_REL = 1, HRTIMER_MODE_PINNED = 2 };
struct hrtimer {
	enum hrtimer_restart (*function)(struct hrtimer *);
	bool active;
};
static inline void hrtimer_setup(struct hrtimer *t,
				 enum hrtimer_restart (*fn)(struct hrtimer *),
				 clockid_t clk_id, enum hrtimer_mode mode)
{
	(void)clk_id;
	(void)mode;
	t->function = fn;
	t->active = false;
}
static inline void hrtimer_init(struct hrtimer *t, clockid_t clk_id, enum hrtimer_mode mode)
{
	(void)clk_id;
	(void)mode;
	t->function = NULL;
	t->active = false;
}
static inline void hrtimer_start(struct hrtimer *t, ktime_t tim, const enum hrtimer_mode mode)
{
	(void)tim;
	(void)mode;
	t->active = true;
}
static inline int hrtimer_cancel(struct hrtimer *t) { t->active = false; return 0; }

struct work_struct {
	void (*func)(struct work_struct *);
	bool pending;
};
#define INIT_WORK(w, fn) do { (w)->func = (fn); (w)->pending = false; } while (0)
static inline bool schedule_work(struct work_struct *w) { w->pending = true; return true; }
static inline bool cancel_work_sync(struct work_struct *w) { bool p = w->pending; w->pending = false; return p; }

/* Time & ktime helpers */
struct timespec64 {
	int64_t tv_sec;
	long tv_nsec;
};
struct system_time_snapshot {
	ktime_t cycles;
	ktime_t real;
	ktime_t raw;
};
#define NSEC_PER_SEC  1000000000LL
#define NSEC_PER_MSEC 1000000LL
#define NSEC_PER_USEC 1000LL
#define USEC_PER_MSEC 1000L
#define USEC_PER_SEC  1000000L
#define MSEC_PER_SEC  1000L

static inline ktime_t ns_to_ktime(u64 ns) { return (ktime_t)ns; }
static inline s64 ktime_to_ns(ktime_t kt) { return (s64)kt; }
static inline s64 ktime_to_us(ktime_t kt) { return (s64)kt / 1000; }
static inline ktime_t ktime_set(const s64 secs, const unsigned long nsecs)
{
	return secs * NSEC_PER_SEC + (s64)nsecs;
}
static inline ktime_t ktime_add_ns(const ktime_t kt, u64 nsval) { return kt + (ktime_t)nsval; }
static inline ktime_t ktime_sub_ns(const ktime_t kt, u64 nsval) { return kt - (ktime_t)nsval; }
static inline ktime_t ktime_sub(const ktime_t lhs, const ktime_t rhs) { return lhs - rhs; }
static inline int ktime_compare(const ktime_t cmp1, const ktime_t cmp2)
{
	return (cmp1 < cmp2) ? -1 : ((cmp1 > cmp2) ? 1 : 0);
}
static inline ktime_t ktime_get(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (ktime_t)ts.tv_sec * NSEC_PER_SEC + ts.tv_nsec;
}
static inline ktime_t ktime_get_real(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	return (ktime_t)ts.tv_sec * NSEC_PER_SEC + ts.tv_nsec;
}
static inline ktime_t ktime_get_clocktai(void) { return ktime_get_real(); }
static inline struct timespec64 ns_to_timespec64(s64 nsec)
{
	struct timespec64 ts = { .tv_sec = nsec / NSEC_PER_SEC, .tv_nsec = (long)(nsec % NSEC_PER_SEC) };
	if (ts.tv_nsec < 0) {
		ts.tv_sec--;
		ts.tv_nsec += (long)NSEC_PER_SEC;
	}
	return ts;
}
static inline struct timespec64 ktime_to_timespec64(ktime_t kt) { return ns_to_timespec64(kt); }
static inline s64 timespec64_to_ns(const struct timespec64 *ts)
{
	return ts->tv_sec * NSEC_PER_SEC + ts->tv_nsec;
}
static inline struct timespec64 timespec64_add(struct timespec64 lhs, struct timespec64 rhs)
{
	return ns_to_timespec64(timespec64_to_ns(&lhs) + timespec64_to_ns(&rhs));
}
static inline int timespec64_compare(const struct timespec64 *lhs, const struct timespec64 *rhs)
{
	if (lhs->tv_sec != rhs->tv_sec)
		return (lhs->tv_sec < rhs->tv_sec) ? -1 : 1;
	if (lhs->tv_nsec != rhs->tv_nsec)
		return (lhs->tv_nsec < rhs->tv_nsec) ? -1 : 1;
	return 0;
}

/* Delays (clamped small in userspace unit tests unless real hardware active) */
extern bool g_dry_fast_delays;
static inline void udelay(unsigned long usecs)
{
	if (!g_dry_fast_delays && usecs > 0)
		usleep((useconds_t)min(usecs, 1000UL));
}
static inline void mdelay(unsigned long msecs) { udelay(msecs * 1000UL); }
static inline void msleep(unsigned int msecs) { udelay((unsigned long)msecs * 1000UL); }
static inline void usleep_range(unsigned long min_us, unsigned long max_us)
{
	(void)max_us;
	udelay(min_us);
}

/* Static keys / jump labels (pruned to plain bool in userspace) */
struct static_key_false { bool enabled; };
#define DECLARE_STATIC_KEY_FALSE(name) extern struct static_key_false name
#define DEFINE_STATIC_KEY_FALSE(name)  struct static_key_false name = { .enabled = false }
#define static_branch_unlikely(x)      unlikely((x)->enabled)
#define static_branch_likely(x)        likely((x)->enabled)
#define static_branch_inc(x)           do { (x)->enabled = true; } while (0)
#define static_branch_dec(x)           do { (x)->enabled = false; } while (0)

/* u64 stats sync */
struct u64_stats_sync { unsigned int seq; };
typedef unsigned int u64_stats_t;
static inline void u64_stats_init(struct u64_stats_sync *syncp) { syncp->seq = 0; }
static inline void u64_stats_update_begin(struct u64_stats_sync *syncp) { syncp->seq++; barrier(); }
static inline void u64_stats_update_end(struct u64_stats_sync *syncp) { barrier(); syncp->seq++; }
static inline unsigned int u64_stats_fetch_begin(const struct u64_stats_sync *syncp) { return READ_ONCE(syncp->seq); }
static inline bool u64_stats_fetch_retry(const struct u64_stats_sync *syncp, unsigned int start)
{
	return READ_ONCE(syncp->seq) != start;
}

/* String / mem helpers */
static inline ssize_t strscpy(char *dest, const char *src, size_t count)
{
	if (count == 0)
		return -E2BIG;
	size_t len = strnlen(src, count);
	if (len == count) {
		memcpy(dest, src, count - 1);
		dest[count - 1] = '\0';
		return -E2BIG;
	}
	memcpy(dest, src, len + 1);
	return (ssize_t)len;
}
static inline bool mem_is_zero(const void *s, size_t n)
{
	const uint8_t *p = (const uint8_t *)s;
	for (size_t i = 0; i < n; i++) {
		if (p[i] != 0)
			return false;
	}
	return true;
}

/* CPU topology & logging */
static inline unsigned int num_online_cpus(void) { return 4U; }
static inline int raw_smp_processor_id(void) { return 0; }
#define smp_processor_id() raw_smp_processor_id()

#define KERN_ERR     ""
#define KERN_WARNING ""
#define KERN_INFO    ""
#define KERN_DEBUG   ""
#define pr_info(fmt, ...)  do { } while (0)
#define pr_err(fmt, ...)   fprintf(stderr, "[igc-island ERR] " fmt, ##__VA_ARGS__)
#define pr_warn(fmt, ...)  do { } while (0)
#define pr_debug(fmt, ...) do { } while (0)
#define net_ratelimit()    0
#define net_err_ratelimited(fmt, ...)   do { } while (0)
#define net_warn_ratelimited(fmt, ...)  do { } while (0)
#define net_info_ratelimited(fmt, ...)  do { } while (0)

#define WARN_ON(condition) ({ \
	int __ret_warn_on = !!(condition); \
	unlikely(__ret_warn_on); })
#define WARN_ON_ONCE(condition) WARN_ON(condition)
#define WARN(condition, fmt, ...) WARN_ON(condition)
#define WARN_ONCE(condition, fmt, ...) WARN_ON(condition)
#define BUILD_BUG_ON(condition) _Static_assert(!(condition), "BUILD_BUG_ON failed")

static inline void prefetch(const void *x) { __builtin_prefetch(x, 0); }
static inline void prefetchw(const void *x) { __builtin_prefetch(x, 1); }
static inline void net_prefetch(void *p) { __builtin_prefetch(p, 0); }

#endif /* LX_EMUL_TYPES_H */

