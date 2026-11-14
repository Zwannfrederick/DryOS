/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_VIRTIO_IDS_H
#define _DRY_LX_VIRTIO_IDS_H

#include <linux/types.h>

#define VIRTIO_ID_RNG       4U
#define VIRTIO_DEV_ANY_ID   0xffffffffU

struct virtio_device_id {
    u32 device;
    u32 vendor;
};

#endif /* _DRY_LX_VIRTIO_IDS_H */
