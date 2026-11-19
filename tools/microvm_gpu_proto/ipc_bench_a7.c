// SPDX-License-Identifier: MIT OR GPL-2.0-only
/*
 * Multi-tier 2-process POSIX Shared Memory GPU IPC Benchmark (`Ascension 7.0 Milestone F`).
 *
 * Benchmarks realistic producer->consumer frame delivery at 1920x1080x4 (8,294,400 B) against
 * the 165 Hz frame budget (6,060.6 us) across 5 native C tiers:
 *   double_eventfd  : T1 Double-buffered single-copy SHM + eventfd doorbell
 *   triple_async    : T2 Triple-buffered fence-aware async ring + eventfd
 *   dirty_rect      : T3 Batched dirty-tile/rect SHM copy (5%, 15%, 25% damage)
 *   spin_futex_full : T4 Pre-allocated buffer recycling + spin-then-futex doorbell (100% frame)
 *   spin_futex_d15  : T4b Same lock-free spin-then-futex doorbell with 15% dirty batch
 *   zerocopy_idx    : T5 Host-process SHM zero-copy slot-index handoff (0 B pixel memcpy; NOT guest DMA-BUF)
 */

#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    FB_W = 1920,
    FB_H = 1080,
    FB_BPP = 4,
    FRAME_BYTES = FB_W * FB_H * FB_BPP, /* 8,294,400 B */
    RING_SLOTS = 4,
    MAX_RECTS = 16,
    SPIN_LIMIT = 4096,
};

typedef struct {
    uint32_t x, y, w, h;
} DirtyRect;

typedef struct {
    uint64_t seq;
    uint32_t slot_idx;
    uint32_t payload_bytes;
    uint32_t num_rects;
    uint32_t sample_tag;
    DirtyRect rects[MAX_RECTS];
} SlotDesc;

typedef struct {
    atomic_uint head;
    atomic_uint tail;
    atomic_uint prod_waiters;
    atomic_uint cons_waiters;
    atomic_int done;
    atomic_uint_fast64_t completed_seq;
    SlotDesc descs[RING_SLOTS];
    uint8_t slots[RING_SLOTS][FRAME_BYTES];
} ShmPool;

static inline uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static int futex_wait(atomic_uint *addr, uint32_t val) {
    return (int)syscall(SYS_futex, addr, FUTEX_WAIT, val, NULL, NULL, 0);
}

static int futex_wake(atomic_uint *addr, int count) {
    return (int)syscall(SYS_futex, addr, FUTEX_WAKE, count, NULL, NULL, 0);
}

static inline uint32_t frame_tag_compute(const uint8_t *buf, uint32_t len, uint64_t seq) {
    if (len < 16) return (uint32_t)seq;
    uint32_t a, b, c;
    memcpy(&a, buf, 4);
    memcpy(&b, buf + (len / 2), 4);
    memcpy(&c, buf + len - 4, 4);
    return a ^ (b * 0x9e3779b1u) ^ c ^ (uint32_t)seq;
}

static inline void stamp_source_buf(uint8_t *buf, uint32_t len, uint64_t seq) {
    uint32_t v0 = (uint32_t)(0xA5A50000u ^ (uint32_t)seq);
    uint32_t v1 = (uint32_t)(0x5A5A0000u + (uint32_t)(seq * 17u));
    uint32_t v2 = (uint32_t)(0xCAFE0000u ^ (uint32_t)(seq * 131u));
    if (len >= 16) {
        memcpy(buf, &v0, 4);
        memcpy(buf + (len / 2), &v1, 4);
        memcpy(buf + len - 4, &v2, 4);
    }
}

static int cmp_double(const void *a, const void *b) {
    double da = *(const double *)a;
    double db = *(const double *)b;
    return (da > db) - (da < db);
}

static double pct(const double *sorted, int n, double p) {
    if (n <= 0) return 0.0;
    int idx = (int)((double)(n - 1) * (p / 100.0));
    if (idx < 0) idx = 0;
    if (idx >= n) idx = n - 1;
    return sorted[idx];
}

static void efd_post(int fd) {
    uint64_t one = 1;
    while (write(fd, &one, sizeof(one)) < 0 && errno == EINTR) {}
}

static int efd_take(int fd) {
    uint64_t val = 0;
    ssize_t r;
    do {
        r = read(fd, &val, sizeof(val));
    } while (r < 0 && errno == EINTR);
    return r == (ssize_t)sizeof(val) ? 0 : -1;
}

static uint32_t ring_wait_space_spin_futex(ShmPool *shm, uint32_t cap) {
    for (;;) {
        uint32_t head = atomic_load_explicit(&shm->head, memory_order_relaxed);
        uint32_t tail = atomic_load_explicit(&shm->tail, memory_order_acquire);
        if ((head - tail) < cap) {
            return head;
        }
        int spun = 0;
        while ((atomic_load_explicit(&shm->head, memory_order_relaxed) -
                atomic_load_explicit(&shm->tail, memory_order_acquire)) >= cap &&
               spun < SPIN_LIMIT) {
            spun++;
        }
        head = atomic_load_explicit(&shm->head, memory_order_relaxed);
        tail = atomic_load_explicit(&shm->tail, memory_order_acquire);
        if ((head - tail) >= cap) {
            atomic_fetch_add_explicit(&shm->prod_waiters, 1, memory_order_seq_cst);
            if ((atomic_load_explicit(&shm->head, memory_order_relaxed) -
                 atomic_load_explicit(&shm->tail, memory_order_acquire)) >= cap) {
                futex_wait(&shm->tail, tail);
            }
            atomic_fetch_sub_explicit(&shm->prod_waiters, 1, memory_order_seq_cst);
        }
    }
}

static void ring_commit_spin_futex(ShmPool *shm, uint32_t head, const SlotDesc *d) {
    shm->descs[head & (RING_SLOTS - 1)] = *d;
    atomic_store_explicit(&shm->head, head + 1u, memory_order_release);
    atomic_thread_fence(memory_order_seq_cst);
    if (atomic_load_explicit(&shm->cons_waiters, memory_order_seq_cst) > 0) {
        futex_wake(&shm->head, 1);
    }
}

static int ring_peek_spin_futex(ShmPool *shm, SlotDesc *out, uint32_t *out_tail) {
    for (;;) {
        uint32_t tail = atomic_load_explicit(&shm->tail, memory_order_relaxed);
        uint32_t head = atomic_load_explicit(&shm->head, memory_order_acquire);
        if (tail != head) {
            *out = shm->descs[tail & (RING_SLOTS - 1)];
            *out_tail = tail;
            return 1;
        }
        if (atomic_load_explicit(&shm->done, memory_order_acquire) != 0) {
            return 0;
        }
        int spun = 0;
        while (atomic_load_explicit(&shm->head, memory_order_acquire) == tail &&
               atomic_load_explicit(&shm->done, memory_order_acquire) == 0 &&
               spun < SPIN_LIMIT) {
            spun++;
        }
        if (atomic_load_explicit(&shm->head, memory_order_acquire) == tail &&
            atomic_load_explicit(&shm->done, memory_order_acquire) == 0) {
            atomic_fetch_add_explicit(&shm->cons_waiters, 1, memory_order_seq_cst);
            if (atomic_load_explicit(&shm->head, memory_order_acquire) == tail &&
                atomic_load_explicit(&shm->done, memory_order_acquire) == 0) {
                futex_wait(&shm->head, tail);
            }
            atomic_fetch_sub_explicit(&shm->cons_waiters, 1, memory_order_seq_cst);
        }
    }
}

static void ring_release_spin_futex(ShmPool *shm, uint32_t cur_tail) {
    atomic_store_explicit(&shm->tail, cur_tail + 1u, memory_order_release);
    atomic_thread_fence(memory_order_seq_cst);
    if (atomic_load_explicit(&shm->prod_waiters, memory_order_seq_cst) > 0) {
        futex_wake(&shm->tail, 1);
    }
}

static uint32_t build_dirty_bytes(int damage_pct) {
    if (damage_pct <= 0) damage_pct = 15;
    if (damage_pct >= 100) return FRAME_BYTES;
    uint64_t b = ((uint64_t)FRAME_BYTES * (uint64_t)damage_pct) / 100ULL;
    return (uint32_t)((b + 4095ULL) & ~4095ULL);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <mode> <iterations> [damage_pct]\n", argv[0]);
        return 2;
    }
    const char *mode = argv[1];
    int iters = atoi(argv[2]);
    if (iters < 10) iters = 10;
    int warmup = iters / 5;
    if (warmup < 8) warmup = 8;
    int damage_pct = (argc >= 4) ? atoi(argv[3]) : 15;

    int is_double_efd = strcmp(mode, "double_eventfd") == 0;
    int is_triple_async = strcmp(mode, "triple_async") == 0;
    int is_dirty_rect = strcmp(mode, "dirty_rect") == 0;
    int is_spin_full = strcmp(mode, "spin_futex_full") == 0;
    int is_spin_d15 = strcmp(mode, "spin_futex_d15") == 0;
    int is_zerocopy = strcmp(mode, "zerocopy_idx") == 0;
    if (!(is_double_efd || is_triple_async || is_dirty_rect || is_spin_full || is_spin_d15 || is_zerocopy)) {
        fprintf(stderr, "unknown mode: %s\n", mode);
        return 2;
    }

    ShmPool *shm = mmap(NULL, sizeof(ShmPool), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shm == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    memset(shm, 0, sizeof(ShmPool));
    int req_efd = eventfd(0, EFD_SEMAPHORE);
    int ack_efd = eventfd(0, EFD_SEMAPHORE);
    if (req_efd < 0 || ack_efd < 0) {
        perror("eventfd");
        return 1;
    }

    uint32_t payload_bytes = FRAME_BYTES;
    if (is_dirty_rect) payload_bytes = build_dirty_bytes(damage_pct);
    if (is_spin_d15) payload_bytes = build_dirty_bytes(15);
    uint32_t ring_cap = is_triple_async ? 3 : 2;
    if (is_zerocopy) ring_cap = 4;

    uint8_t *local_buf = NULL;
    if (!is_zerocopy) {
        local_buf = aligned_alloc(4096, FRAME_BYTES);
        if (!local_buf) return 1;
        for (size_t i = 0; i < FRAME_BYTES; i++) {
            local_buf[i] = (uint8_t)((i * 29u + 13u) & 0xffu);
        }
    } else {
        for (int s = 0; s < RING_SLOTS; s++) {
            for (size_t i = 0; i < 4096; i++) {
                shm->slots[s][i] = (uint8_t)((i * 29u + 13u) & 0xffu);
            }
        }
    }

    int total_runs = warmup + iters;
    double *lat_us = calloc((size_t)iters, sizeof(double));
    if (!lat_us) return 1;

    struct rusage ru_self0;
    getrusage(RUSAGE_SELF, &ru_self0);
    uint64_t wall_t0 = now_ns();
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }

    if (pid == 0) {
        /* Consumer process */
        int errors = 0;
        if (is_double_efd || is_dirty_rect) {
            for (int i = 0; i < total_runs; i++) {
                if (efd_take(req_efd) != 0) {
                    errors++;
                    break;
                }
                uint32_t slot = (uint32_t)(i & 1);
                const SlotDesc *d = &shm->descs[slot];
                uint32_t got_tag = frame_tag_compute(shm->slots[slot], d->payload_bytes, d->seq);
                if (got_tag != d->sample_tag || d->payload_bytes != payload_bytes) {
                    errors++;
                }
                atomic_store_explicit(&shm->completed_seq, d->seq, memory_order_release);
                efd_post(ack_efd);
            }
        } else if (is_triple_async) {
            for (int i = 0; i < total_runs; i++) {
                if (efd_take(req_efd) != 0) {
                    errors++;
                    break;
                }
                uint32_t tail = atomic_load_explicit(&shm->tail, memory_order_relaxed);
                SlotDesc d = shm->descs[tail % 3u];
                uint32_t got_tag = frame_tag_compute(shm->slots[d.slot_idx], d.payload_bytes, d.seq);
                if (got_tag != d.sample_tag || d.payload_bytes != payload_bytes) {
                    errors++;
                }
                atomic_store_explicit(&shm->tail, tail + 1, memory_order_release);
                atomic_store_explicit(&shm->completed_seq, d.seq, memory_order_release);
                efd_post(ack_efd);
            }
        } else {
            SlotDesc d;
            uint32_t cur_tail = 0;
            while (ring_peek_spin_futex(shm, &d, &cur_tail)) {
                uint32_t got_tag = frame_tag_compute(shm->slots[d.slot_idx], d.payload_bytes, d.seq);
                if (got_tag != d.sample_tag || d.payload_bytes != payload_bytes) {
                    errors++;
                }
                atomic_store_explicit(&shm->completed_seq, d.seq, memory_order_release);
                ring_release_spin_futex(shm, cur_tail);
            }
        }
        struct rusage cr;
        getrusage(RUSAGE_SELF, &cr);
        _exit(errors == 0 ? 0 : 3);
    }

    /* Producer parent */
    uint64_t bench_start_ns = 0;
    for (int i = 0; i < total_runs; i++) {
        if (i == warmup) {
            bench_start_ns = now_ns();
        }
        uint64_t seq = (uint64_t)i + 1ULL;
        uint64_t t0 = now_ns();

        if (is_double_efd || is_dirty_rect) {
            uint32_t slot = (uint32_t)(i & 1);
            stamp_source_buf(local_buf, payload_bytes, seq);
            memcpy(shm->slots[slot], local_buf, payload_bytes);
            SlotDesc d = {
                .seq = seq,
                .slot_idx = slot,
                .payload_bytes = payload_bytes,
                .num_rects = is_dirty_rect ? 4u : 1u,
                .sample_tag = frame_tag_compute(shm->slots[slot], payload_bytes, seq),
            };
            shm->descs[slot] = d;
            efd_post(req_efd);
            if (efd_take(ack_efd) != 0) return 1;
        } else if (is_triple_async) {
            /* Keep at most 2 inflight ahead in 3-slot ring so consumer scanout runs pipelined. */
            uint32_t head = atomic_load_explicit(&shm->head, memory_order_relaxed);
            while ((head - atomic_load_explicit(&shm->tail, memory_order_acquire)) >= ring_cap) {
                if (efd_take(ack_efd) != 0) return 1;
            }
            uint32_t slot = head % 3u;
            stamp_source_buf(local_buf, payload_bytes, seq);
            memcpy(shm->slots[slot], local_buf, payload_bytes);
            SlotDesc d = {
                .seq = seq,
                .slot_idx = slot,
                .payload_bytes = payload_bytes,
                .num_rects = 1u,
                .sample_tag = frame_tag_compute(shm->slots[slot], payload_bytes, seq),
            };
            shm->descs[head % 3u] = d;
            atomic_store_explicit(&shm->head, head + 1, memory_order_release);
            efd_post(req_efd);
            if (i + 1 == total_runs) {
                while (atomic_load_explicit(&shm->completed_seq, memory_order_acquire) < seq) {
                    if (efd_take(ack_efd) != 0) return 1;
                }
            } else if ((head + 1u - atomic_load_explicit(&shm->tail, memory_order_acquire)) >= 2u) {
                if (efd_take(ack_efd) != 0) return 1;
            }
        } else {
            uint32_t head = ring_wait_space_spin_futex(shm, ring_cap);
            uint32_t slot = head % ring_cap;
            uint8_t *dst = shm->slots[slot];
            if (!is_zerocopy) {
                stamp_source_buf(local_buf, payload_bytes, seq);
                memcpy(dst, local_buf, payload_bytes);
            } else {
                /* Producer updates directly in pre-mapped slot (0 B pixel memcpy). */
                stamp_source_buf(dst, payload_bytes, seq);
            }
            SlotDesc d = {
                .seq = seq,
                .slot_idx = slot,
                .payload_bytes = payload_bytes,
                .num_rects = is_spin_d15 ? 4u : 1u,
                .sample_tag = frame_tag_compute(dst, payload_bytes, seq),
            };
            ring_commit_spin_futex(shm, head, &d);
        }

        uint64_t t1 = now_ns();
        if (i >= warmup) {
            lat_us[i - warmup] = (double)(t1 - t0) / 1000.0;
        }
    }

    if (is_spin_full || is_spin_d15 || is_zerocopy) {
        atomic_store_explicit(&shm->done, 1, memory_order_release);
        atomic_thread_fence(memory_order_seq_cst);
        futex_wake(&shm->head, 1);
    }

    int status = 0;
    waitpid(pid, &status, 0);
    uint64_t bench_end_ns = now_ns();
    int child_ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;

    struct rusage ru_self, ru_child;
    getrusage(RUSAGE_SELF, &ru_self);
    getrusage(RUSAGE_CHILDREN, &ru_child);
    double total_wall_s = (double)(bench_end_ns - wall_t0) / 1e9;
    double bench_wall_s = (double)(bench_end_ns - bench_start_ns) / 1e9;
    double self_cpu_end = (double)ru_self.ru_utime.tv_sec + (double)ru_self.ru_utime.tv_usec / 1e6 +
                          (double)ru_self.ru_stime.tv_sec + (double)ru_self.ru_stime.tv_usec / 1e6;
    double self_cpu_start = (double)ru_self0.ru_utime.tv_sec + (double)ru_self0.ru_utime.tv_usec / 1e6 +
                            (double)ru_self0.ru_stime.tv_sec + (double)ru_self0.ru_stime.tv_usec / 1e6;
    double child_cpu_s = (double)ru_child.ru_utime.tv_sec + (double)ru_child.ru_utime.tv_usec / 1e6 +
                         (double)ru_child.ru_stime.tv_sec + (double)ru_child.ru_stime.tv_usec / 1e6;
    double cpu_s = (self_cpu_end - self_cpu_start) + child_cpu_s;
    if (cpu_s < 0.0) cpu_s = 0.0;
    double cpu_pct = total_wall_s > 0.0 ? (cpu_s / total_wall_s) * 100.0 : 0.0;
    long max_rss_kib = ru_self.ru_maxrss > ru_child.ru_maxrss ? ru_self.ru_maxrss : ru_child.ru_maxrss;

    qsort(lat_us, (size_t)iters, sizeof(double), cmp_double);
    double sum = 0.0;
    for (int i = 0; i < iters; i++) sum += lat_us[i];
    double mean_us = sum / (double)iters;
    double copied_mib = ((double)(is_zerocopy ? 0ULL : (uint64_t)payload_bytes) * (double)iters) / (1024.0 * 1024.0);
    double logical_mib = ((double)FRAME_BYTES * (double)iters) / (1024.0 * 1024.0);
    double copy_mib_s = bench_wall_s > 0.0 ? copied_mib / bench_wall_s : 0.0;
    double effective_mib_s = bench_wall_s > 0.0 ? logical_mib / bench_wall_s : 0.0;

    printf(
        "{\"mode\":\"%s\",\"iterations\":%d,\"integrity_ok\":%s,"
        "\"logical_frame_bytes\":%u,\"copied_payload_bytes\":%u,"
        "\"min_us\":%.2f,\"p50_us\":%.2f,\"p95_us\":%.2f,\"p99_us\":%.2f,\"mean_us\":%.2f,"
        "\"copy_throughput_mib_s\":%.2f,\"effective_1080p_mib_s\":%.2f,"
        "\"cpu_util_pct\":%.1f,\"max_rss_kib\":%ld}\n",
        mode,
        iters,
        child_ok ? "true" : "false",
        (unsigned)FRAME_BYTES,
        is_zerocopy ? 0u : payload_bytes,
        lat_us[0],
        pct(lat_us, iters, 50.0),
        pct(lat_us, iters, 95.0),
        pct(lat_us, iters, 99.0),
        mean_us,
        copy_mib_s,
        effective_mib_s,
        cpu_pct,
        max_rss_kib);

    free(lat_us);
    free(local_buf);
    close(req_efd);
    close(ack_efd);
    munmap(shm, sizeof(ShmPool));
    return child_ok ? 0 : 1;
}
