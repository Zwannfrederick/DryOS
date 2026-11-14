/* SPDX-License-Identifier: MIT */
/* Freestanding prototype compatibility validator for NVIDIA open-gpu `os-interface.h` -> DRY OS. */

#include <stdint.h>
#include <stddef.h>

#if defined(HAVE_NVIDIA_OS_INTERFACE_H)
#include "os-interface.h"
#else
typedef uint8_t NvU8;
typedef uint16_t NvU16;
typedef uint32_t NvU32;
typedef int32_t NvS32;
typedef uint64_t NvU64;
typedef uint8_t NvBool;
typedef uint32_t NV_STATUS;
#define NV_OK 0U
#define NV_ERR_GENERIC 0xFFFFU
#define NV_ERR_INVALID_ARGUMENT 0x0001U
#define NV_TRUE 1U
#define NV_FALSE 0U
#define NV_API_CALL
#endif

static uint8_t g_nv_slab[16384];
static size_t g_nv_slab_used = 0;

NV_STATUS NV_API_CALL os_alloc_mem(void **address, NvU64 size) {
    if (!address || size == 0U) {
        return NV_ERR_INVALID_ARGUMENT;
    }
    size_t aligned = ((size_t)size + 15U) & ~(size_t)15U;
    if (g_nv_slab_used + aligned > sizeof(g_nv_slab)) {
        *address = NULL;
        return NV_ERR_GENERIC;
    }
    *address = &g_nv_slab[g_nv_slab_used];
    g_nv_slab_used += aligned;
    return NV_OK;
}

void NV_API_CALL os_free_mem(void *address) {
    (void)address;
}

NvU64 NV_API_CALL os_get_monotonic_time_ns(void) {
    return 1000000ULL;
}

NV_STATUS NV_API_CALL os_delay(NvU32 milli_seconds) {
    (void)milli_seconds;
    return NV_OK;
}

NV_STATUS NV_API_CALL os_delay_us(NvU32 micro_seconds) {
    (void)micro_seconds;
    return NV_OK;
}

void *NV_API_CALL os_mem_copy(void *dst, const void *src, NvU32 size) {
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    for (NvU32 i = 0; i < size; i++) {
        d[i] = s[i];
    }
    return dst;
}

void *NV_API_CALL os_mem_set(void *dst, NvU8 byte, NvU32 size) {
    uint8_t *d = (uint8_t *)dst;
    for (NvU32 i = 0; i < size; i++) {
        d[i] = byte;
    }
    return dst;
}

NvS32 NV_API_CALL os_mem_cmp(const NvU8 *p0, const NvU8 *p1, NvU32 size) {
    for (NvU32 i = 0; i < size; i++) {
        if (p0[i] != p1[i]) {
            return (p0[i] < p1[i]) ? -1 : 1;
        }
    }
    return 0;
}

NV_STATUS NV_API_CALL os_pci_read_byte(void *handle, NvU32 offset, NvU8 *val) {
    (void)handle;
    (void)offset;
    if (val) {
        *val = 0;
    }
    return NV_OK;
}

NV_STATUS NV_API_CALL os_pci_read_word(void *handle, NvU32 offset, NvU16 *val) {
    (void)handle;
    (void)offset;
    if (val) {
        *val = 0;
    }
    return NV_OK;
}

NV_STATUS NV_API_CALL os_pci_read_dword(void *handle, NvU32 offset, NvU32 *val) {
    (void)handle;
    (void)offset;
    if (val) {
        *val = 0;
    }
    return NV_OK;
}

void *NV_API_CALL os_map_kernel_space(NvU64 start, NvU64 size_bytes, NvU32 mode) {
    (void)start;
    (void)size_bytes;
    (void)mode;
    return NULL;
}

void NV_API_CALL os_unmap_kernel_space(void *addr, NvU64 size_bytes) {
    (void)addr;
    (void)size_bytes;
}

NV_STATUS NV_API_CALL os_alloc_spinlock(void **lock) {
    static uint32_t dummy_lock = 0;
    if (lock) {
        *lock = &dummy_lock;
    }
    return NV_OK;
}

void NV_API_CALL os_free_spinlock(void *lock) {
    (void)lock;
}

NvU64 NV_API_CALL os_acquire_spinlock(void *lock) {
    (void)lock;
    return 0ULL;
}

void NV_API_CALL os_release_spinlock(void *lock, NvU64 old_irql) {
    (void)lock;
    (void)old_irql;
}

void *NV_API_CALL os_alloc_semaphore(NvU32 initial_value) {
    static uint32_t dummy_sem = 0;
    dummy_sem = initial_value;
    return &dummy_sem;
}

void NV_API_CALL os_free_semaphore(void *sem) {
    (void)sem;
}

NV_STATUS NV_API_CALL os_acquire_semaphore(void *sem) {
    (void)sem;
    return NV_OK;
}

NV_STATUS NV_API_CALL os_release_semaphore(void *sem) {
    (void)sem;
    return NV_OK;
}

NV_STATUS NV_API_CALL os_get_is_openrm(NvBool *is_openrm) {
    if (!is_openrm) {
        return NV_ERR_INVALID_ARGUMENT;
    }
    *is_openrm = NV_TRUE;
    return NV_OK;
}
