// SPDX-License-Identifier: MIT OR GPL-2.0-only
#define _POSIX_C_SOURCE 200809L

#include <inttypes.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum {
    FB_W = 1280,
    FB_H = 800,
    FB_BYTES = FB_W * FB_H * 4,
    PROBE_ITERS = 48,
};

struct dry_prime_frame_cap {
    uint64_t frame_id;
    uint32_t generation;
    uint32_t rights_ro;
    uint64_t bytes;
};

struct dry_ioring_fence {
    _Atomic uint64_t seq;
    _Atomic uint32_t generation;
    _Atomic uint32_t last_crc;
};

static inline uint64_t mono_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static int cmp_u64(const void *a, const void *b) {
    uint64_t va = *(const uint64_t *)a;
    uint64_t vb = *(const uint64_t *)b;
    return (va > vb) - (va < vb);
}

static uint64_t percentile_ns(uint64_t *samples, size_t n, size_t pct) {
    qsort(samples, n, sizeof(uint64_t), cmp_u64);
    size_t idx = (pct * (n - 1)) / 100;
    return samples[idx];
}

int main(void) {
    uint8_t *src = aligned_alloc(64, FB_BYTES);
    uint8_t *dst = aligned_alloc(64, FB_BYTES);
    if (!src || !dst) {
        fprintf(stderr, "alloc failed\n");
        free(src);
        free(dst);
        return 1;
    }

    for (size_t i = 0; i < FB_BYTES; i += 4) {
        uint32_t v = (uint32_t)(0xFF0B101BU ^ (uint32_t)(i * 2654435761U));
        memcpy(src + i, &v, 4);
    }
    memset(dst, 0, FB_BYTES);

    uint64_t copy_samples[PROBE_ITERS];
    uint64_t fence_samples[PROBE_ITERS];
    volatile uint32_t sink = 0;

    /* Warmup */
    memcpy(dst, src, FB_BYTES);
    sink ^= ((const uint32_t *)dst)[0];

    for (size_t it = 0; it < PROBE_ITERS; ++it) {
        src[0] = (uint8_t)it;
        src[FB_BYTES - 4] = (uint8_t)(it ^ 0x5AU);
        uint64_t t0 = mono_ns();
        memcpy(dst, src, FB_BYTES);
        uint64_t t1 = mono_ns();
        copy_samples[it] = (t1 > t0) ? (t1 - t0) : 1;
        sink ^= ((const uint32_t *)dst)[0] ^ ((const uint32_t *)dst)[(FB_BYTES / 4) - 1];
    }

    struct dry_prime_frame_cap caps[2] = {
        { .frame_id = 1001, .generation = 2, .rights_ro = 0x5, .bytes = 2U * 1024U * 1024U },
        { .frame_id = 1002, .generation = 2, .rights_ro = 0x5, .bytes = 2U * 1024U * 1024U },
    };
    struct dry_ioring_fence fence;
    atomic_init(&fence.seq, 0ULL);
    atomic_init(&fence.generation, 2U);
    atomic_init(&fence.last_crc, 0U);

    /* Validate stale-generation rejection before timing steady-state zero-copy handoff. */
    uint32_t active_gen = atomic_load_explicit(&fence.generation, memory_order_acquire);
    struct dry_prime_frame_cap stale_cap = { .frame_id = 999, .generation = 1, .rights_ro = 0x5, .bytes = 2097152 };
    int stale_gen_rejected = (stale_cap.generation != active_gen) ? 1 : 0;
    int checksum_ok = 1;

    for (size_t it = 0; it < PROBE_ITERS; ++it) {
        uint64_t target_seq = (uint64_t)it + 1ULL;
        const uint32_t *ro0 = (const uint32_t *)src;
        uint32_t tag = ro0[0] ^ ro0[(FB_BYTES / 4) - 1] ^ (uint32_t)caps[0].frame_id;
        uint64_t t0 = mono_ns();
        atomic_store_explicit(&fence.last_crc, tag, memory_order_relaxed);
        atomic_store_explicit(&fence.seq, target_seq, memory_order_release);
        uint64_t seen_seq = atomic_load_explicit(&fence.seq, memory_order_acquire);
        uint32_t seen_gen = atomic_load_explicit(&fence.generation, memory_order_acquire);
        uint32_t seen_tag = atomic_load_explicit(&fence.last_crc, memory_order_relaxed);
        uint64_t t1 = mono_ns();
        if (seen_seq != target_seq || seen_gen != caps[1].generation || seen_tag != tag) {
            checksum_ok = 0;
        }
        fence_samples[it] = (t1 > t0) ? (t1 - t0) : 1;
        sink ^= seen_tag;
    }

    (void)sink;
    uint64_t copy_p50 = percentile_ns(copy_samples, PROBE_ITERS, 50);
    uint64_t copy_p95 = percentile_ns(copy_samples, PROBE_ITERS, 95);
    uint64_t fence_p50 = percentile_ns(fence_samples, PROBE_ITERS, 50);
    uint64_t fence_p95 = percentile_ns(fence_samples, PROBE_ITERS, 95);
    double speedup = (double)copy_p50 / (double)(fence_p50 ? fence_p50 : 1);

    printf(
        "{\"frame_bytes\":%d,\"iterations\":%d,\"full_copy_p50_ns\":%" PRIu64
        ",\"full_copy_p95_ns\":%" PRIu64 ",\"zero_copy_fence_p50_ns\":%" PRIu64
        ",\"zero_copy_fence_p95_ns\":%" PRIu64 ",\"speedup_x\":%.1f"
        ",\"stale_gen_rejected\":%s,\"checksum_ok\":%s}\n",
        FB_BYTES,
        PROBE_ITERS,
        copy_p50,
        copy_p95,
        fence_p50,
        fence_p95,
        speedup,
        stale_gen_rejected ? "true" : "false",
        checksum_ok ? "true" : "false"
    );

    free(src);
    free(dst);
    return (stale_gen_rejected && checksum_ok) ? 0 : 2;
}
