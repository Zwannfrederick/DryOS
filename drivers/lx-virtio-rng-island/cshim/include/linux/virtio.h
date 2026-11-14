/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_VIRTIO_H
#define _DRY_LX_VIRTIO_H

#include <linux/scatterlist.h>
#include <linux/types.h>
#include <linux/virtio_config.h>
#include <linux/virtio_ids.h>

struct virtio_device;

struct virtqueue {
    struct virtio_device *vdev;
    void (*callback)(struct virtqueue *vq);
    const char *name;
    u16 num;
    u16 notify_off;
    u16 last_used_idx;
    u16 avail_idx;
    void *tokens[16];
};

struct virtio_config_ops {
    void (*del_vqs)(struct virtio_device *vdev);
};

struct device_driver {
    const char *name;
};

struct virtio_device {
    void *priv;
    const struct virtio_config_ops *config;
};

struct virtio_driver {
    struct device_driver driver;
    const struct virtio_device_id *id_table;
    int (*probe)(struct virtio_device *vdev);
    void (*scan)(struct virtio_device *vdev);
    void (*remove)(struct virtio_device *vdev);
    int (*freeze)(struct virtio_device *vdev);
    int (*restore)(struct virtio_device *vdev);
};

struct virtqueue *virtio_find_single_vq(
    struct virtio_device *vdev,
    void (*callback)(struct virtqueue *vq),
    const char *name
);
void virtio_device_ready(struct virtio_device *vdev);
void virtio_reset_device(struct virtio_device *vdev);
int virtqueue_add_inbuf(
    struct virtqueue *vq,
    struct scatterlist *sg,
    unsigned int num,
    void *data,
    gfp_t gfp
);
bool virtqueue_kick(struct virtqueue *vq);
void *virtqueue_get_buf(struct virtqueue *vq, unsigned int *len);

struct virtio_driver *lx_shim_get_virtio_driver(void);
#define module_virtio_driver(__drv) \
    struct virtio_driver *lx_shim_get_virtio_driver(void) { return &(__drv); }

#endif /* _DRY_LX_VIRTIO_H */
