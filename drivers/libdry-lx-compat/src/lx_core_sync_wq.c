/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#include <linux/compat_base.h>
#include <linux/completion.h>
#include <linux/delay.h>
#include <linux/jiffies.h>
#include <linux/mutex.h>
#include <linux/netdevice.h>
#include <linux/spinlock.h>
#include <linux/workqueue.h>

#define DEVM_POOL_BYTES 16384U
#define MAX_WQ_ITEMS 32U
#define MAX_NAPI_ITEMS 8U

struct wq_entry {
    bool in_use;
    struct work_struct *work;
    struct delayed_work *dwork;
    unsigned long due_jiffies;
};

static uint8_t g_devm_pool[DEVM_POOL_BYTES];
static size_t g_devm_used = 0;

static struct wq_entry g_wq_ring[MAX_WQ_ITEMS];
static struct workqueue_struct g_default_wq = { .name = "dry_lx_wq", .active = true };
static struct napi_struct *g_napi_list[MAX_NAPI_ITEMS];

extern void dry_lx_rust_log(const char *tag, const char *msg);

__attribute__((weak)) uint64_t dry_lx_rust_tsc_khz(void) {
    return 3000000ULL;
}

static inline uint64_t read_tsc(void) {
    uint32_t lo = 0, hi = 0;
    __asm__ __volatile__("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | (uint64_t)lo;
}

void *dry_lx_memset(void *dst, int c, size_t n) {
    uint8_t *p = (uint8_t *)dst;
    uint8_t v = (uint8_t)c;
    for (size_t i = 0; i < n; i++) {
        p[i] = v;
    }
    return dst;
}

void *dry_lx_memcpy(void *dst, const void *src, size_t n) {
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    for (size_t i = 0; i < n; i++) {
        d[i] = s[i];
    }
    return dst;
}

int dry_lx_memcmp(const void *a, const void *b, size_t n) {
    const uint8_t *pa = (const uint8_t *)a;
    const uint8_t *pb = (const uint8_t *)b;
    for (size_t i = 0; i < n; i++) {
        if (pa[i] != pb[i]) {
            return (pa[i] < pb[i]) ? -1 : 1;
        }
    }
    return 0;
}

size_t dry_lx_strlen(const char *s) {
    size_t n = 0;
    while (s && s[n] != '\0') {
        n++;
    }
    return n;
}

size_t strscpy(char *dest, const char *src, size_t count) {
    if (!dest || count == 0) {
        return 0;
    }
    size_t i = 0;
    if (src) {
        while (i + 1 < count && src[i] != '\0') {
            dest[i] = src[i];
            i++;
        }
    }
    dest[i] = '\0';
    return i;
}

#undef memset
#undef memcpy
#undef memcmp
__attribute__((weak)) void *memset(void *dst, int c, size_t n) {
    return dry_lx_memset(dst, c, n);
}

__attribute__((weak)) void *memcpy(void *dst, const void *src, size_t n) {
    return dry_lx_memcpy(dst, src, n);
}

__attribute__((weak)) int memcmp(const void *a, const void *b, size_t n) {
    return dry_lx_memcmp(a, b, n);
}

void dry_lx_log(const char *tag, const char *fmt, ...) {
    dry_lx_rust_log(tag ? tag : "DRV", fmt ? fmt : "");
}

void dry_lx_bug(const char *file, int line) {
    (void)line;
    dry_lx_rust_log("BUG", file ? file : "kernel_compat");
    __asm__ __volatile__("ud2");
    for (;;) {
    }
}

void dry_lx_devm_reset(void) {
    g_devm_used = 0;
    dry_lx_memset(g_wq_ring, 0, sizeof(g_wq_ring));
    dry_lx_memset(g_napi_list, 0, sizeof(g_napi_list));
}

void *dry_lx_devm_zalloc(struct device *dev, size_t size) {
    (void)dev;
    size_t aligned = (size + 15U) & ~(size_t)15U;
    if (aligned == 0 || g_devm_used + aligned > DEVM_POOL_BYTES) {
        return NULL;
    }
    void *ptr = &g_devm_pool[g_devm_used];
    g_devm_used += aligned;
    dry_lx_memset(ptr, 0, aligned);
    return ptr;
}

void dry_lx_spin_lock_init(spinlock_t *l) {
    if (l) {
        l->lock = 0U;
    }
}

void dry_lx_spin_lock(spinlock_t *l) {
    if (!l) {
        return;
    }
    while (__atomic_exchange_n(&l->lock, 1U, __ATOMIC_ACQUIRE) != 0U) {
        __builtin_ia32_pause();
    }
}

void dry_lx_spin_unlock(spinlock_t *l) {
    if (l) {
        __atomic_store_n(&l->lock, 0U, __ATOMIC_RELEASE);
    }
}

void mutex_init(struct mutex *lock) {
    if (lock) {
        lock->locked = 0U;
        lock->contentions = 0U;
    }
}

void mutex_lock(struct mutex *lock) {
    if (!lock) {
        return;
    }
    while (__atomic_exchange_n(&lock->locked, 1U, __ATOMIC_ACQUIRE) != 0U) {
        lock->contentions++;
        __builtin_ia32_pause();
    }
}

int mutex_trylock(struct mutex *lock) {
    if (!lock) {
        return 0;
    }
    return (__atomic_exchange_n(&lock->locked, 1U, __ATOMIC_ACQUIRE) == 0U) ? 1 : 0;
}

void mutex_unlock(struct mutex *lock) {
    if (lock) {
        __atomic_store_n(&lock->locked, 0U, __ATOMIC_RELEASE);
    }
}

void init_completion(struct completion *x) {
    if (x) {
        __atomic_store_n(&x->done, 0U, __ATOMIC_RELEASE);
    }
}

void reinit_completion(struct completion *x) {
    if (x) {
        __atomic_store_n(&x->done, 0U, __ATOMIC_RELEASE);
    }
}

void complete(struct completion *x) {
    if (x) {
        __atomic_store_n(&x->done, 1U, __ATOMIC_RELEASE);
    }
}

unsigned long wait_for_completion_timeout(struct completion *x, unsigned long timeout_jiffies) {
    if (!x) {
        return 0UL;
    }
    unsigned long start = dry_lx_jiffies();
    unsigned long deadline = start + timeout_jiffies;
    for (;;) {
        if (__atomic_load_n(&x->done, __ATOMIC_ACQUIRE) != 0U) {
            unsigned long now = dry_lx_jiffies();
            return (deadline > now) ? (deadline - now) : 1UL;
        }
        if (time_after(dry_lx_jiffies(), deadline)) {
            return 0UL;
        }
        dry_lx_wq_run_tick(16);
        __builtin_ia32_pause();
    }
}

int wait_for_completion_killable(struct completion *x) {
    return wait_for_completion_timeout(x, msecs_to_jiffies(2000U)) ? 0 : -ETIMEDOUT;
}

unsigned long dry_lx_jiffies(void) {
    uint64_t khz = dry_lx_rust_tsc_khz();
    if (khz == 0ULL) {
        khz = 3000000ULL;
    }
    /* HZ = 1000 => 1 jiffy per 1 ms = khz TSC cycles. */
    return (unsigned long)(read_tsc() / khz);
}

void udelay(unsigned long usecs) {
    uint64_t khz = dry_lx_rust_tsc_khz();
    if (khz == 0ULL) {
        khz = 3000000ULL;
    }
    uint64_t cycles = ((uint64_t)usecs * khz) / 1000ULL;
    uint64_t start = read_tsc();
    while ((read_tsc() - start) < cycles) {
        __builtin_ia32_pause();
    }
}

void mdelay(unsigned long msecs) {
    for (unsigned long i = 0; i < msecs; i++) {
        udelay(1000UL);
    }
}

long schedule_timeout_uninterruptible(long timeout_jiffies) {
    if (timeout_jiffies <= 0) {
        return 0;
    }
    udelay(50UL);
    return 0;
}

struct workqueue_struct *alloc_workqueue(const char *fmt, unsigned int flags, int max_active) {
    (void)flags;
    (void)max_active;
    g_default_wq.name = fmt ? fmt : "dry_lx_wq";
    g_default_wq.active = true;
    return &g_default_wq;
}

void destroy_workqueue(struct workqueue_struct *wq) {
    if (wq) {
        flush_workqueue(wq);
        wq->active = false;
    }
}

void INIT_WORK(struct work_struct *work, work_func_t func) {
    if (work) {
        work->func = func;
        work->pending = false;
    }
}

void INIT_DELAYED_WORK(struct delayed_work *dwork, work_func_t func) {
    if (dwork) {
        INIT_WORK(&dwork->work, func);
        dwork->due_jiffies = 0UL;
        dwork->timer_pending = false;
    }
}

bool queue_work(struct workqueue_struct *wq, struct work_struct *work) {
    if ((wq && !wq->active) || !work || !work->func || work->pending) {
        return false;
    }
    for (size_t i = 0; i < MAX_WQ_ITEMS; i++) {
        if (!g_wq_ring[i].in_use) {
            g_wq_ring[i].in_use = true;
            g_wq_ring[i].work = work;
            g_wq_ring[i].dwork = NULL;
            g_wq_ring[i].due_jiffies = 0UL;
            work->pending = true;
            return true;
        }
    }
    return false;
}

bool queue_delayed_work(struct workqueue_struct *wq, struct delayed_work *dwork, unsigned long delay) {
    if ((wq && !wq->active) || !dwork || !dwork->work.func || dwork->timer_pending) {
        return false;
    }
    for (size_t i = 0; i < MAX_WQ_ITEMS; i++) {
        if (!g_wq_ring[i].in_use) {
            g_wq_ring[i].in_use = true;
            g_wq_ring[i].work = &dwork->work;
            g_wq_ring[i].dwork = dwork;
            g_wq_ring[i].due_jiffies = dry_lx_jiffies() + delay;
            dwork->due_jiffies = g_wq_ring[i].due_jiffies;
            dwork->timer_pending = true;
            dwork->work.pending = true;
            return true;
        }
    }
    return false;
}

bool cancel_delayed_work_sync(struct delayed_work *dwork) {
    if (!dwork) {
        return false;
    }
    bool cancelled = false;
    for (size_t i = 0; i < MAX_WQ_ITEMS; i++) {
        if (g_wq_ring[i].in_use && g_wq_ring[i].dwork == dwork) {
            g_wq_ring[i].in_use = false;
            g_wq_ring[i].work = NULL;
            g_wq_ring[i].dwork = NULL;
            cancelled = true;
        }
    }
    dwork->timer_pending = false;
    dwork->work.pending = false;
    return cancelled;
}

void flush_workqueue(struct workqueue_struct *wq) {
    (void)wq;
    (void)dry_lx_wq_run_tick(64);
}

int dry_lx_wq_register_napi(struct napi_struct *napi) {
    if (!napi) {
        return -EINVAL;
    }
    for (size_t i = 0; i < MAX_NAPI_ITEMS; i++) {
        if (g_napi_list[i] == napi) {
            return 0;
        }
    }
    for (size_t i = 0; i < MAX_NAPI_ITEMS; i++) {
        if (g_napi_list[i] == NULL) {
            g_napi_list[i] = napi;
            return 0;
        }
    }
    return -ENOMEM;
}

void dry_lx_wq_unregister_napi(struct napi_struct *napi) {
    for (size_t i = 0; i < MAX_NAPI_ITEMS; i++) {
        if (g_napi_list[i] == napi) {
            g_napi_list[i] = NULL;
        }
    }
}

int dry_lx_wq_run_tick(int budget) {
    int total_work = 0;
    unsigned long now = dry_lx_jiffies();

    for (size_t i = 0; i < MAX_WQ_ITEMS; i++) {
        if (!g_wq_ring[i].in_use) {
            continue;
        }
        if (g_wq_ring[i].dwork != NULL && time_after(g_wq_ring[i].due_jiffies, now)) {
            continue;
        }
        struct work_struct *w = g_wq_ring[i].work;
        struct delayed_work *dw = g_wq_ring[i].dwork;
        g_wq_ring[i].in_use = false;
        g_wq_ring[i].work = NULL;
        g_wq_ring[i].dwork = NULL;
        if (dw) {
            dw->timer_pending = false;
        }
        if (w) {
            w->pending = false;
            if (w->func) {
                w->func(w);
                total_work++;
            }
        }
    }

    int rem = (budget > 0) ? budget : 16;
    for (size_t i = 0; i < MAX_NAPI_ITEMS && rem > 0; i++) {
        struct napi_struct *napi = g_napi_list[i];
        if (napi && napi->enabled && napi->scheduled && napi->poll) {
            napi->missed = false;
            int done = napi->poll(napi, rem);
            total_work += done;
            rem -= done;
        }
    }
    return total_work;
}
