/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_CRC32_H
#define _DRY_LINUX_CRC32_H

#include <linux/types.h>

u32 ether_crc(int length, unsigned char *data);

#endif
