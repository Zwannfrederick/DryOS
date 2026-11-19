/* SPDX-License-Identifier: GPL-2.0-or-later */
#include <dry_lx_drm.h>

int32_t dry_lx_bochs_probe(
    uint16_t vendor_id,
    uint16_t device_id,
    uint16_t subvendor,
    uint16_t subdevice,
    uint16_t bdf,
    uint64_t bar0_size,
    uint8_t bar0_flags,
    uint64_t bar2_size,
    uint8_t bar2_flags,
    struct dry_lx_drm_probe_info *out) {
    struct dry_lx_pci_bar_init bars[6];
    dry_lx_memset(bars, 0, sizeof(bars));
    bars[0].size_bytes = bar0_size;
    bars[0].flags = bar0_flags;
    bars[2].size_bytes = bar2_size;
    bars[2].flags = bar2_flags;
    return dry_lx_drm_probe_pci_device(
        vendor_id,
        device_id,
        subvendor,
        subdevice,
        bdf,
        2U,
        0U,
        bars,
        out);
}
