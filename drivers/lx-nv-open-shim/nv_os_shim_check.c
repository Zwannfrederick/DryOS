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
#define NV_ERR_INVALID_ARGUMENT 0x0001U
#define NV_ERR_NOT_SUPPORTED 0x0056U
#define NV_ERR_GENERIC 0xFFFFU
#define NV_TRUE 1U
#define NV_FALSE 0U
#define NV_API_CALL
#endif

enum dry_nv_shim_status {
    NV_SHIM_IMPLEMENTED = 1,
    NV_SHIM_PARTIAL = 2,
    NV_SHIM_STUB_ENOSYS = 3,
    NV_SHIM_COMPILE_ONLY = 4,
};

struct dry_nv_shim_entry {
    const char *symbol;
    enum dry_nv_shim_status status;
};

static const struct dry_nv_shim_entry g_nv_shim_table[22] = {
    { "os_alloc_mem", NV_SHIM_PARTIAL },
    { "os_free_mem", NV_SHIM_STUB_ENOSYS },
    { "os_get_monotonic_time_ns", NV_SHIM_COMPILE_ONLY },
    { "os_delay", NV_SHIM_STUB_ENOSYS },
    { "os_delay_us", NV_SHIM_STUB_ENOSYS },
    { "os_mem_copy", NV_SHIM_IMPLEMENTED },
    { "os_mem_set", NV_SHIM_IMPLEMENTED },
    { "os_mem_cmp", NV_SHIM_IMPLEMENTED },
    { "os_pci_read_byte", NV_SHIM_PARTIAL },
    { "os_pci_read_word", NV_SHIM_PARTIAL },
    { "os_pci_read_dword", NV_SHIM_PARTIAL },
    { "os_map_kernel_space", NV_SHIM_COMPILE_ONLY },
    { "os_unmap_kernel_space", NV_SHIM_STUB_ENOSYS },
    { "os_alloc_spinlock", NV_SHIM_STUB_ENOSYS },
    { "os_free_spinlock", NV_SHIM_STUB_ENOSYS },
    { "os_acquire_spinlock", NV_SHIM_COMPILE_ONLY },
    { "os_release_spinlock", NV_SHIM_COMPILE_ONLY },
    { "os_alloc_semaphore", NV_SHIM_COMPILE_ONLY },
    { "os_free_semaphore", NV_SHIM_STUB_ENOSYS },
    { "os_acquire_semaphore", NV_SHIM_STUB_ENOSYS },
    { "os_release_semaphore", NV_SHIM_STUB_ENOSYS },
    { "os_get_is_openrm", NV_SHIM_IMPLEMENTED },
};

void nv_os_shim_audit_summary(
    uint32_t *impl_out, uint32_t *partial_out, uint32_t *stub_out, uint32_t *compile_only_out) {
    uint32_t impl = 0U, partial = 0U, stub = 0U, compile_only = 0U;
    for (size_t i = 0; i < (sizeof(g_nv_shim_table) / sizeof(g_nv_shim_table[0])); i++) {
        switch (g_nv_shim_table[i].status) {
            case NV_SHIM_IMPLEMENTED: impl++; break;
            case NV_SHIM_PARTIAL: partial++; break;
            case NV_SHIM_STUB_ENOSYS: stub++; break;
            case NV_SHIM_COMPILE_ONLY: compile_only++; break;
        }
    }
    if (impl_out) *impl_out = impl;
    if (partial_out) *partial_out = partial;
    if (stub_out) *stub_out = stub;
    if (compile_only_out) *compile_only_out = compile_only;
}

__attribute__((weak)) int32_t dry_nv_rust_pci_cfg_rw(
    uint16_t off, uint8_t width, uint8_t is_write, uint32_t *val) {
    (void)off;
    (void)width;
    (void)is_write;
    (void)val;
    return -38; /* -ENOSYS when no PCI capability bridge is bound */
}

static uint8_t g_nv_slab[16384];
static size_t g_nv_slab_used = 0;

/* NV_SHIM_PARTIAL: bounded 16 KiB bump slab with no reclamation yet */
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

/* NV_SHIM_STUB_ENOSYS: bump slab does not track individual frees */
void NV_API_CALL os_free_mem(void *address) {
    (void)address;
}

/* NV_SHIM_COMPILE_ONLY: monotonic clock bridge not bound */
NvU64 NV_API_CALL os_get_monotonic_time_ns(void) {
    return 0ULL;
}

/* NV_SHIM_STUB_ENOSYS: sleeping timer not wired */
NV_STATUS NV_API_CALL os_delay(NvU32 milli_seconds) {
    (void)milli_seconds;
    return NV_ERR_NOT_SUPPORTED;
}

/* NV_SHIM_STUB_ENOSYS: busy/udelay timer not wired */
NV_STATUS NV_API_CALL os_delay_us(NvU32 micro_seconds) {
    (void)micro_seconds;
    return NV_ERR_NOT_SUPPORTED;
}

/* NV_SHIM_IMPLEMENTED */
void *NV_API_CALL os_mem_copy(void *dst, const void *src, NvU32 size) {
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    for (NvU32 i = 0; i < size; i++) {
        d[i] = s[i];
    }
    return dst;
}

/* NV_SHIM_IMPLEMENTED */
void *NV_API_CALL os_mem_set(void *dst, NvU8 byte, NvU32 size) {
    uint8_t *d = (uint8_t *)dst;
    for (NvU32 i = 0; i < size; i++) {
        d[i] = byte;
    }
    return dst;
}

/* NV_SHIM_IMPLEMENTED */
NvS32 NV_API_CALL os_mem_cmp(const NvU8 *p0, const NvU8 *p1, NvU32 size) {
    for (NvU32 i = 0; i < size; i++) {
        if (p0[i] != p1[i]) {
            return (p0[i] < p1[i]) ? -1 : 1;
        }
    }
    return 0;
}

/* NV_SHIM_PARTIAL: routes to weak DRY PCI config bridge; fails with NV_ERR_NOT_SUPPORTED when unwired */
NV_STATUS NV_API_CALL os_pci_read_byte(void *handle, NvU32 offset, NvU8 *val) {
    (void)handle;
    if (!val || offset > 0xfffU) {
        return NV_ERR_INVALID_ARGUMENT;
    }
    uint32_t raw = 0U;
    if (dry_nv_rust_pci_cfg_rw((uint16_t)offset, 1U, 0U, &raw) != 0) {
        return NV_ERR_NOT_SUPPORTED;
    }
    *val = (NvU8)(raw & 0xffU);
    return NV_OK;
}

/* NV_SHIM_PARTIAL */
NV_STATUS NV_API_CALL os_pci_read_word(void *handle, NvU32 offset, NvU16 *val) {
    (void)handle;
    if (!val || offset > 0xfffU) {
        return NV_ERR_INVALID_ARGUMENT;
    }
    uint32_t raw = 0U;
    if (dry_nv_rust_pci_cfg_rw((uint16_t)offset, 2U, 0U, &raw) != 0) {
        return NV_ERR_NOT_SUPPORTED;
    }
    *val = (NvU16)(raw & 0xffffU);
    return NV_OK;
}

/* NV_SHIM_PARTIAL */
NV_STATUS NV_API_CALL os_pci_read_dword(void *handle, NvU32 offset, NvU32 *val) {
    (void)handle;
    if (!val || offset > 0xffcU) {
        return NV_ERR_INVALID_ARGUMENT;
    }
    uint32_t raw = 0U;
    if (dry_nv_rust_pci_cfg_rw((uint16_t)offset, 4U, 0U, &raw) != 0) {
        return NV_ERR_NOT_SUPPORTED;
    }
    *val = raw;
    return NV_OK;
}

/* NV_SHIM_COMPILE_ONLY: OpenRM BAR MMIO mapping not wired */
void *NV_API_CALL os_map_kernel_space(NvU64 start, NvU64 size_bytes, NvU32 mode) {
    (void)start;
    (void)size_bytes;
    (void)mode;
    return NULL;
}

/* NV_SHIM_STUB_ENOSYS */
void NV_API_CALL os_unmap_kernel_space(void *addr, NvU64 size_bytes) {
    (void)addr;
    (void)size_bytes;
}

/* NV_SHIM_STUB_ENOSYS: real spinlock backing absent; fails closed instead of returning dummy NV_OK */
NV_STATUS NV_API_CALL os_alloc_spinlock(void **lock) {
    if (lock) {
        *lock = NULL;
    }
    return NV_ERR_NOT_SUPPORTED;
}

/* NV_SHIM_STUB_ENOSYS */
void NV_API_CALL os_free_spinlock(void *lock) {
    (void)lock;
}

/* NV_SHIM_COMPILE_ONLY */
NvU64 NV_API_CALL os_acquire_spinlock(void *lock) {
    (void)lock;
    return 0ULL;
}

/* NV_SHIM_COMPILE_ONLY */
void NV_API_CALL os_release_spinlock(void *lock, NvU64 old_irql) {
    (void)lock;
    (void)old_irql;
}

/* NV_SHIM_COMPILE_ONLY: real semaphore backing absent */
void *NV_API_CALL os_alloc_semaphore(NvU32 initial_value) {
    (void)initial_value;
    return NULL;
}

/* NV_SHIM_STUB_ENOSYS */
void NV_API_CALL os_free_semaphore(void *sem) {
    (void)sem;
}

/* NV_SHIM_STUB_ENOSYS */
NV_STATUS NV_API_CALL os_acquire_semaphore(void *sem) {
    (void)sem;
    return NV_ERR_NOT_SUPPORTED;
}

/* NV_SHIM_STUB_ENOSYS */
NV_STATUS NV_API_CALL os_release_semaphore(void *sem) {
    (void)sem;
    return NV_ERR_NOT_SUPPORTED;
}

/* NV_SHIM_IMPLEMENTED */
NV_STATUS NV_API_CALL os_get_is_openrm(NvBool *is_openrm) {
    if (!is_openrm) {
        return NV_ERR_INVALID_ARGUMENT;
    }
    *is_openrm = NV_TRUE;
    return NV_OK;
}
