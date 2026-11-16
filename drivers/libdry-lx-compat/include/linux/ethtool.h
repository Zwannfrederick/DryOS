/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_ETHTOOL_H
#define _DRY_LINUX_ETHTOOL_H

#include <linux/netdevice.h>

#define ETH_GSTRING_LEN 32
#define ETH_SS_STATS 1

#define SUPPORTED_10baseT_Half (1U << 0)
#define SUPPORTED_10baseT_Full (1U << 1)
#define SUPPORTED_100baseT_Half (1U << 2)
#define SUPPORTED_100baseT_Full (1U << 3)
#define SUPPORTED_1000baseT_Half (1U << 4)
#define SUPPORTED_1000baseT_Full (1U << 5)
#define SUPPORTED_Autoneg (1U << 6)
#define SUPPORTED_TP (1U << 7)
#define SUPPORTED_MII (1U << 9)

#define ADVERTISED_10baseT_Half (1U << 0)
#define ADVERTISED_10baseT_Full (1U << 1)
#define ADVERTISED_100baseT_Half (1U << 2)
#define ADVERTISED_100baseT_Full (1U << 3)
#define ADVERTISED_1000baseT_Half (1U << 4)
#define ADVERTISED_1000baseT_Full (1U << 5)
#define ADVERTISED_Autoneg (1U << 6)
#define ADVERTISED_TP (1U << 7)
#define ADVERTISED_MII (1U << 9)

#define PORT_MII 0x03
#define XCVR_INTERNAL 0x00
#define ETH_MDIO_SUPPORTS_C22 1
#define AUTONEG_DISABLE 0x00
#define AUTONEG_ENABLE 0x01
#define SPEED_10 10
#define SPEED_100 100
#define SPEED_1000 1000
#define SPEED_UNKNOWN -1
#define DUPLEX_HALF 0x00
#define DUPLEX_FULL 0x01

#define WAKE_PHY (1U << 0)
#define WAKE_UCAST (1U << 1)
#define WAKE_MCAST (1U << 2)
#define WAKE_BCAST (1U << 3)
#define WAKE_MAGIC (1U << 5)

struct ethtool_cmd {
    u32 supported;
    u32 advertising;
    u16 speed;
    u8 duplex;
    u8 port;
    u8 phy_address;
    u8 transceiver;
    u8 autoneg;
    u8 mdio_support;
    u32 lp_advertising;
    u16 speed_hi;
};

static inline void ethtool_cmd_speed_set(struct ethtool_cmd *ep, u32 speed) {
    ep->speed = (u16)(speed & 0xffffU);
    ep->speed_hi = (u16)(speed >> 16);
}

static inline u32 ethtool_cmd_speed(const struct ethtool_cmd *ep) {
    return ((u32)ep->speed_hi << 16) | ep->speed;
}

struct ethtool_link_settings {
    u32 speed;
    u8 duplex;
    u8 port;
    u8 phy_address;
    u8 autoneg;
    u8 mdio_support;
};

struct ethtool_link_ksettings {
    struct ethtool_link_settings base;
    struct {
        unsigned long supported[2];
        unsigned long advertising[2];
        unsigned long lp_advertising[2];
    } link_modes;
};

static inline void ethtool_convert_legacy_u32_to_link_mode(unsigned long *dst, u32 legacy) {
    dst[0] = (unsigned long)legacy;
    dst[1] = 0UL;
}

static inline bool ethtool_convert_link_mode_to_legacy_u32(u32 *legacy, const unsigned long *src) {
    *legacy = (u32)src[0];
    return true;
}

struct ethtool_drvinfo {
    char driver[32];
    char version[32];
    char bus_info[32];
};

struct ethtool_regs {
    u32 version;
    u32 len;
};

struct ethtool_wolinfo {
    u32 supported;
    u32 wolopts;
};

struct ethtool_eeprom {
    u32 magic;
    u32 offset;
    u32 len;
};

struct ethtool_stats {
    u32 n_stats;
};

struct ethtool_ringparam {
    u32 rx_max_pending;
    u32 tx_max_pending;
    u32 rx_pending;
    u32 tx_pending;
};

struct kernel_ethtool_ringparam {
    u32 rx_buf_len;
};

struct netlink_ext_ack {
    const char *_msg;
};

struct ethtool_ops {
    void (*get_drvinfo)(struct net_device *dev, struct ethtool_drvinfo *info);
    int (*get_regs_len)(struct net_device *dev);
    void (*get_regs)(struct net_device *dev, struct ethtool_regs *regs, void *p);
    void (*get_wol)(struct net_device *dev, struct ethtool_wolinfo *wol);
    int (*set_wol)(struct net_device *dev, struct ethtool_wolinfo *wol);
    int (*nway_reset)(struct net_device *dev);
    u32 (*get_link)(struct net_device *dev);
    int (*get_eeprom_len)(struct net_device *dev);
    int (*get_eeprom)(struct net_device *dev, struct ethtool_eeprom *ee, u8 *data);
    int (*set_eeprom)(struct net_device *dev, struct ethtool_eeprom *ee, u8 *data);
    u32 (*get_msglevel)(struct net_device *dev);
    void (*set_msglevel)(struct net_device *dev, u32 level);
    void (*get_strings)(struct net_device *dev, u32 stringset, u8 *data);
    void (*get_ethtool_stats)(struct net_device *dev, struct ethtool_stats *stats, u64 *data);
    int (*get_sset_count)(struct net_device *dev, int sset);
    void (*get_ringparam)(struct net_device *dev, struct ethtool_ringparam *ring,
                          struct kernel_ethtool_ringparam *kring, struct netlink_ext_ack *extack);
    int (*get_link_ksettings)(struct net_device *dev, struct ethtool_link_ksettings *cmd);
    int (*set_link_ksettings)(struct net_device *dev, const struct ethtool_link_ksettings *cmd);
};

u32 ethtool_op_get_link(struct net_device *dev);

#endif /* _DRY_LINUX_ETHTOOL_H */
