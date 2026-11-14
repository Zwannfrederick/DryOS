/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_APERTURE_H
#define _DRY_LINUX_APERTURE_H

struct pci_dev;

static inline int aperture_remove_conflicting_pci_devices(struct pci_dev *pdev, const char *name) {
    (void)pdev;
    (void)name;
    return 0;
}

#endif
