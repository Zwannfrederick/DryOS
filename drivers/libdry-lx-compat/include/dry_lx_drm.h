/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LX_DRM_H
#define _DRY_LX_DRM_H

#include <linux/pci.h>

struct dry_lx_drm_probe_info {
    uint32_t edid_ok;
    uint32_t pref_w;
    uint32_t pref_h;
    uint32_t mapped_bar0_ok;
    uint32_t mapped_bar2_ok;
};

int32_t dry_lx_drm_probe_pci_device(
    uint16_t vendor_id,
    uint16_t device_id,
    uint16_t subvendor,
    uint16_t subdevice,
    uint16_t bdf,
    uint8_t revision,
    uint16_t dev_flags,
    const struct dry_lx_pci_bar_init *bars,
    struct dry_lx_drm_probe_info *out);

int32_t dry_lx_drm_modeset(uint32_t w, uint32_t h);

int32_t dry_lx_drm_commit_dmabuf(
    const void *src_ptr,
    uint32_t w,
    uint32_t h,
    uint32_t pitch,
    int32_t x,
    int32_t y,
    int32_t dw,
    int32_t dh);

void dry_lx_drm_shutdown(void);

#endif /* _DRY_LX_DRM_H */
