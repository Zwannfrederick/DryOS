/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_MII_H
#define _DRY_LINUX_MII_H

#include <linux/ethtool.h>

#define MII_BMCR 0x00
#define MII_BMSR 0x01
#define MII_ADVERTISE 0x04
#define MII_LPA 0x05
#define MII_EXPANSION 0x06
#define MII_CTRL1000 0x09
#define MII_STAT1000 0x0a
#define MII_ESTATUS 0x0f

#define BMCR_SPEED1000 0x0040U
#define BMCR_FULLDPLX 0x0100U
#define BMCR_ANRESTART 0x0200U
#define BMCR_ANENABLE 0x1000U
#define BMCR_SPEED100 0x2000U
#define BMCR_RESET 0x8000U

#define BMSR_LSTATUS 0x0004U
#define BMSR_ANEGCOMPLETE 0x0020U
#define BMSR_ESTATEN 0x0100U

#define ADVERTISE_CSMA 0x0001U
#define ADVERTISE_10HALF 0x0020U
#define ADVERTISE_10FULL 0x0040U
#define ADVERTISE_100HALF 0x0080U
#define ADVERTISE_100FULL 0x0100U
#define ADVERTISE_100BASE4 0x0200U
#define ADVERTISE_FULL (ADVERTISE_100FULL | ADVERTISE_10FULL | ADVERTISE_CSMA)
#define ADVERTISE_ALL (ADVERTISE_10HALF | ADVERTISE_10FULL | ADVERTISE_100HALF | ADVERTISE_100FULL)
#define ADVERTISE_1000HALF 0x0100U
#define ADVERTISE_1000FULL 0x0200U

#define LPA_10HALF 0x0020U
#define LPA_10FULL 0x0040U
#define LPA_100HALF 0x0080U
#define LPA_100FULL 0x0100U
#define LPA_1000HALF 0x0400U
#define LPA_1000FULL 0x0800U

#define ESTATUS_1000_THALF 0x1000U
#define ESTATUS_1000_TFULL 0x2000U

#define SIOCGMIIPHY 0x8947
#define SIOCGMIIREG 0x8948
#define SIOCSMIIREG 0x8949

struct mii_if_info {
    int phy_id;
    int advertising;
    int phy_id_mask;
    int reg_num_mask;
    unsigned int full_duplex : 1;
    unsigned int force_media : 1;
    unsigned int supports_gmii : 1;
    struct net_device *dev;
    int (*mdio_read)(struct net_device *dev, int phy_id, int location);
    void (*mdio_write)(struct net_device *dev, int phy_id, int location, int val);
};

struct mii_ioctl_data {
    u16 phy_id;
    u16 reg_num;
    u16 val_in;
    u16 val_out;
};

static inline struct mii_ioctl_data *if_mii(struct ifreq *rq) {
    return (struct mii_ioctl_data *)rq->ifr_data;
}

static inline u32 mii_lpa_to_ethtool_lpa_t(u32 lpa) {
    u32 r = 0;
    if (lpa & LPA_10HALF) r |= ADVERTISED_10baseT_Half;
    if (lpa & LPA_10FULL) r |= ADVERTISED_10baseT_Full;
    if (lpa & LPA_100HALF) r |= ADVERTISED_100baseT_Half;
    if (lpa & LPA_100FULL) r |= ADVERTISED_100baseT_Full;
    return r;
}

static inline u32 mii_ctrl1000_to_ethtool_adv_t(u32 ctrl1000) {
    u32 r = 0;
    if (ctrl1000 & ADVERTISE_1000HALF) r |= ADVERTISED_1000baseT_Half;
    if (ctrl1000 & ADVERTISE_1000FULL) r |= ADVERTISED_1000baseT_Full;
    return r;
}

static inline u32 mii_stat1000_to_ethtool_lpa_t(u32 stat1000) {
    u32 r = 0;
    if (stat1000 & LPA_1000HALF) r |= ADVERTISED_1000baseT_Half;
    if (stat1000 & LPA_1000FULL) r |= ADVERTISED_1000baseT_Full;
    return r;
}

static inline u32 ethtool_adv_to_mii_adv_t(u32 ethadv) {
    u32 r = 0;
    if (ethadv & ADVERTISED_10baseT_Half) r |= ADVERTISE_10HALF;
    if (ethadv & ADVERTISED_10baseT_Full) r |= ADVERTISE_10FULL;
    if (ethadv & ADVERTISED_100baseT_Half) r |= ADVERTISE_100HALF;
    if (ethadv & ADVERTISED_100baseT_Full) r |= ADVERTISE_100FULL;
    return r;
}

static inline u32 ethtool_adv_to_mii_ctrl1000_t(u32 ethadv) {
    u32 r = 0;
    if (ethadv & ADVERTISED_1000baseT_Half) r |= ADVERTISE_1000HALF;
    if (ethadv & ADVERTISED_1000baseT_Full) r |= ADVERTISE_1000FULL;
    return r;
}

static inline unsigned int mii_nway_result(unsigned int negotiated) {
    unsigned int ret;
    if (negotiated & LPA_100FULL) ret = LPA_100FULL;
    else if (negotiated & LPA_100HALF) ret = LPA_100HALF;
    else if (negotiated & LPA_10FULL) ret = LPA_10FULL;
    else ret = LPA_10HALF;
    return ret;
}

int mii_link_ok(struct mii_if_info *mii);
int mii_nway_restart(struct mii_if_info *mii);
void mii_ethtool_gset(struct mii_if_info *mii, struct ethtool_cmd *ecmd);
void mii_ethtool_get_link_ksettings(struct mii_if_info *mii, struct ethtool_link_ksettings *cmd);
int mii_ethtool_sset(struct mii_if_info *mii, struct ethtool_cmd *ecmd);
int mii_ethtool_set_link_ksettings(struct mii_if_info *mii, const struct ethtool_link_ksettings *cmd);
void mii_check_link(struct mii_if_info *mii);
unsigned int mii_check_media(struct mii_if_info *mii, unsigned int ok_to_print, unsigned int init_media);
int mii_check_gmii_support(struct mii_if_info *mii);
int generic_mii_ioctl(struct mii_if_info *mii_if, struct mii_ioctl_data *mii_data,
                      int cmd, unsigned int *duplex_chg_out);

#endif /* _DRY_LINUX_MII_H */
