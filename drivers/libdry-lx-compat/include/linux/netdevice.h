/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_NETDEVICE_H
#define _DRY_LINUX_NETDEVICE_H

#include <linux/compat_base.h>
#include <linux/pci.h>
#include <linux/skbuff.h>
#include <linux/spinlock.h>

#define IFNAMSIZ 16
#define ETH_ALEN 6
#define ETH_HLEN 14
#define ETH_DATA_LEN 1500
#define MAX_ADDR_LEN 32

#define ETH_P_IP 0x0800U
#define ETH_P_ARP 0x0806U
#define ETH_P_IPV6 0x86DDU

#define IFF_PROMISC 0x100U
#define IFF_ALLMULTI 0x200U

#define NETDEV_TX_OK 0x00
#define NETDEV_TX_BUSY 0x10

#define NETIF_F_SG (1ULL << 0)
#define NETIF_F_IP_CSUM (1ULL << 1)
#define NETIF_F_RXCSUM (1ULL << 2)
#define NETIF_F_TSO (1ULL << 3)
#define NETIF_F_HIGHDMA (1ULL << 4)
#define NETIF_F_HW_VLAN_CTAG_TX (1ULL << 5)
#define NETIF_F_HW_VLAN_CTAG_RX (1ULL << 6)

#define NETIF_MSG_DRV 0x0001U
#define NETIF_MSG_PROBE 0x0002U
#define NETIF_MSG_LINK 0x0004U
#define NETIF_MSG_IFDOWN 0x0010U
#define NETIF_MSG_IFUP 0x0020U
#define NETIF_MSG_RX_ERR 0x0040U
#define NETIF_MSG_TX_ERR 0x0080U
#define NETIF_MSG_TX_QUEUED 0x0100U
#define NETIF_MSG_INTR 0x0200U
#define NETIF_MSG_TX_DONE 0x0400U
#define NETIF_MSG_RX_STATUS 0x0800U

typedef int netdev_tx_t;
typedef u64 netdev_features_t;

struct sockaddr {
    unsigned short sa_family;
    char sa_data[14];
};

struct ifreq {
    char ifr_name[IFNAMSIZ];
    u8 ifr_data[24];
};

struct net_device_stats {
    unsigned long rx_packets;
    unsigned long tx_packets;
    unsigned long rx_bytes;
    unsigned long tx_bytes;
    unsigned long rx_errors;
    unsigned long tx_errors;
    unsigned long rx_dropped;
    unsigned long tx_dropped;
    unsigned long collisions;
    unsigned long rx_length_errors;
    unsigned long rx_crc_errors;
    unsigned long rx_frame_errors;
    unsigned long rx_fifo_errors;
    unsigned long rx_missed_errors;
    unsigned long tx_aborted_errors;
    unsigned long tx_carrier_errors;
    unsigned long tx_fifo_errors;
    unsigned long tx_window_errors;
};

struct netdev_hw_addr {
    struct netdev_hw_addr *next;
    u8 addr[MAX_ADDR_LEN];
};

struct napi_struct {
    struct net_device *dev;
    int (*poll)(struct napi_struct *napi, int budget);
    int weight;
    bool enabled;
    bool scheduled;
    bool missed;
};

struct ethtool_ops;

struct net_device_ops {
    int (*ndo_open)(struct net_device *dev);
    int (*ndo_stop)(struct net_device *dev);
    int (*ndo_validate_addr)(struct net_device *dev);
    int (*ndo_set_mac_address)(struct net_device *dev, void *addr);
    void (*ndo_set_rx_mode)(struct net_device *dev);
    struct net_device_stats *(*ndo_get_stats)(struct net_device *dev);
    int (*ndo_eth_ioctl)(struct net_device *dev, struct ifreq *ifr, int cmd);
    netdev_tx_t (*ndo_start_xmit)(struct sk_buff *skb, struct net_device *dev);
    void (*ndo_tx_timeout)(struct net_device *dev, unsigned int txqueue);
    int (*ndo_set_features)(struct net_device *dev, netdev_features_t features);
    int (*ndo_change_mtu)(struct net_device *dev, int new_mtu);
    netdev_features_t (*ndo_features_check)(struct sk_buff *skb, struct net_device *dev,
                                            netdev_features_t features);
    void (*ndo_poll_controller)(struct net_device *dev);
};

struct net_device {
    char name[IFNAMSIZ];
    void *priv;
    struct device *parent_dev;
    const struct net_device_ops *netdev_ops;
    const struct ethtool_ops *ethtool_ops;
    u8 dev_addr[MAX_ADDR_LEN] __attribute__((aligned(8)));
    unsigned char addr_len;
    unsigned int mtu;
    unsigned int min_mtu;
    unsigned int max_mtu;
    unsigned int flags;
    int watchdog_timeo;
    netdev_features_t features;
    netdev_features_t hw_features;
    netdev_features_t vlan_features;
    struct net_device_stats stats;
    struct netdev_hw_addr *mc_head;
    unsigned int mc_count;
    bool running;
    bool present;
    bool carrier;
    bool queue_stopped;
    struct napi_struct *napi_primary;
};

static inline void *netdev_priv(const struct net_device *dev) {
    return dev->priv;
}

static inline void SET_NETDEV_DEV(struct net_device *net, struct device *pdev) {
    net->parent_dev = pdev;
}

static inline bool netif_running(const struct net_device *dev) {
    return dev->running;
}

static inline bool netif_device_present(const struct net_device *dev) {
    return dev->present;
}

static inline void netif_device_detach(struct net_device *dev) {
    dev->present = false;
}

static inline void netif_device_attach(struct net_device *dev) {
    dev->present = true;
}

static inline void netif_start_queue(struct net_device *dev) {
    dev->queue_stopped = false;
}

static inline void netif_stop_queue(struct net_device *dev) {
    dev->queue_stopped = true;
}

static inline void netif_wake_queue(struct net_device *dev) {
    dev->queue_stopped = false;
}

static inline void netif_carrier_on(struct net_device *dev) {
    dev->carrier = true;
}

static inline void netif_carrier_off(struct net_device *dev) {
    dev->carrier = false;
}

static inline bool netif_carrier_ok(const struct net_device *dev) {
    return dev->carrier;
}

static inline int netdev_mc_count(const struct net_device *dev) {
    return (int)dev->mc_count;
}

#define netdev_for_each_mc_addr(ha, dev) \
    for ((ha) = (dev)->mc_head; (ha) != NULL; (ha) = (ha)->next)

#define netif_msg_link(p) (((p)->msg_enable & NETIF_MSG_LINK) != 0U)
#define netif_dbg(priv, type, dev, fmt, ...) ((void)(priv), (void)(dev))
#define netdev_info(dev, fmt, ...) ((void)(dev), dry_lx_log("NET", fmt, ##__VA_ARGS__))
#define netdev_err(dev, fmt, ...) ((void)(dev), dry_lx_log("NET-ERR", fmt, ##__VA_ARGS__))
#define netdev_warn(dev, fmt, ...) ((void)(dev), dry_lx_log("NET-WRN", fmt, ##__VA_ARGS__))
#define netdev_WARN_ONCE(dev, fmt, ...) ((void)(dev), dry_lx_log("NET-WRN", fmt, ##__VA_ARGS__))
#define dev_info(dev, fmt, ...) ((void)(dev), dry_lx_log("DEV", fmt, ##__VA_ARGS__))
#define dev_err(dev, fmt, ...) ((void)(dev), dry_lx_log("DEV-ERR", fmt, ##__VA_ARGS__))

void free_netdev(struct net_device *dev);
int register_netdev(struct net_device *dev);
void unregister_netdev(struct net_device *dev);
void netdev_sent_queue(struct net_device *dev, unsigned int bytes);
void netdev_completed_queue(struct net_device *dev, unsigned int pkts, unsigned int bytes);
void netdev_reset_queue(struct net_device *dev);

void netif_napi_add_weight(struct net_device *dev, struct napi_struct *napi,
                           int (*poll)(struct napi_struct *, int), int weight);
static inline void netif_napi_add(struct net_device *dev, struct napi_struct *napi,
                                  int (*poll)(struct napi_struct *, int)) {
    netif_napi_add_weight(dev, napi, poll, 64);
}
void napi_enable(struct napi_struct *napi);
void napi_disable(struct napi_struct *napi);
bool napi_schedule_prep(struct napi_struct *napi);
void __napi_schedule(struct napi_struct *napi);
void napi_schedule_irqoff(struct napi_struct *napi);
bool napi_complete_done(struct napi_struct *napi, int work_done);
struct sk_buff *napi_alloc_skb(struct napi_struct *napi, unsigned int length);
int napi_gro_receive(struct napi_struct *napi, struct sk_buff *skb);
int netif_receive_skb(struct sk_buff *skb);

struct dry_lx_net_probe_info {
    u8 mac[6];
    u8 link_up;
    u8 bar1_mapped;
    u16 bar1_suboff;
    u16 irq_is_msi;
    u32 mtu;
};

struct dry_lx_net_stats {
    u64 tx_packets;
    u64 rx_packets;
    u64 tx_bytes;
    u64 rx_bytes;
    u64 arp_replies;
    u64 udp_echo_pkts;
    u64 irqs_handled;
    u8 mac[6];
    u8 gw_mac_known;
    u8 link_up;
};

int32_t dry_lx_net_probe(u16 vendor_id, u16 device_id, u16 subvendor, u16 subdevice,
                         u16 bdf, u8 revision, u16 dev_flags,
                         const struct dry_lx_pci_bar_init *bars,
                         struct dry_lx_net_probe_info *out);
int32_t dry_lx_net_send_arp_who_has(u32 target_ip_be);
int32_t dry_lx_net_poll_step(int budget, struct dry_lx_net_stats *out);
void dry_lx_net_get_stats(struct dry_lx_net_stats *out);
void dry_lx_net_shutdown(void);
void dry_lx_net_set_raw_mode(u8 en);
int32_t dry_lx_net_tx_raw(const u8 *frame, u32 len);
int32_t dry_lx_net_rx_pop(u8 *out, u32 cap);

#endif /* _DRY_LINUX_NETDEVICE_H */
