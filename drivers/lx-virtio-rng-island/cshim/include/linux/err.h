/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_ERR_H
#define _DRY_LX_ERR_H

#include <linux/types.h>

#define EPERM   1
#define EINTR   4
#define EIO     5
#define ENXIO   6
#define EAGAIN  11
#define ENOMEM  12
#define EBUSY   16
#define ENODEV  19
#define EINVAL  22
#define ENOSPC  28

#define MAX_ERRNO 4095

static inline void *ERR_PTR(long error) {
    return (void *)error;
}

static inline long PTR_ERR(const void *ptr) {
    return (long)ptr;
}

static inline bool IS_ERR(const void *ptr) {
    unsigned long v = (unsigned long)ptr;
    return v >= (unsigned long)(-MAX_ERRNO);
}

#endif /* _DRY_LX_ERR_H */
