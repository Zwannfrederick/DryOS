/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Pruned Subsystem & Unsupported Behavior Link Stubs for `igc`
 *
 * Satisfies cross-TU calls from `igc_main.c` into the 7 omitted optional `igc`
 * files (`igc_ethtool.c`, `igc_ptp.c`, `igc_tsn.c`, `igc_xdp.c`, `igc_leds.c`,
 * `igc_dump.c`, `igc_diag.c`), preserving 0 lines modified in upstream `igc`.
 * Note: `igc_write_rss_indir_tbl` (located in upstream `igc_ethtool.c`) writes
 * hardware `IGC_RETA` registers during `igc_configure_rx`, so its hardware
 * register loop is implemented faithfully here.
 */
#include "igc.h"
#include "igc_tsn.h"
#include "igc_xdp.h"

DEFINE_STATIC_KEY_FALSE(igc_fpe_enabled);

/* Hardware RSS indirection table programming (called by `igc_configure_rx`) */
void igc_write_rss_indir_tbl(struct igc_adapter *adapter)
{
	struct igc_hw *hw = &adapter->hw;
	u32 reg = IGC_RETA(0);
	for (int i = 0; i < IGC_RETA_SIZE; i += 4, reg += 4) {
		u32 val = ((u32)adapter->rss_indir_tbl[i + 3] << 24) |
			  ((u32)adapter->rss_indir_tbl[i + 2] << 16) |
			  ((u32)adapter->rss_indir_tbl[i + 1] << 8) |
			  (u32)adapter->rss_indir_tbl[i];
		wr32(reg, val);
	}
}

/* Pruned Ethtool / Dump / LEDs stubs (`feature_prune`) */
void igc_ethtool_set_ops(struct net_device *netdev) { (void)netdev; }
void igc_rings_dump(struct igc_adapter *adapter) { (void)adapter; }
void igc_regs_dump(struct igc_adapter *adapter) { (void)adapter; }
int igc_led_setup(struct igc_adapter *adapter) { (void)adapter; return 0; }
void igc_led_free(struct igc_adapter *adapter) { (void)adapter; }

/* Pruned IEEE 1588 PTP hardware clock stubs (`feature_prune`) */
void igc_ptp_init(struct igc_adapter *adapter) { (void)adapter; }
void igc_ptp_reset(struct igc_adapter *adapter) { (void)adapter; }
void igc_ptp_suspend(struct igc_adapter *adapter) { (void)adapter; }
void igc_ptp_stop(struct igc_adapter *adapter) { (void)adapter; }
void igc_ptp_tx_hang(struct igc_adapter *adapter) { (void)adapter; }
void igc_ptp_tx_tstamp_event(struct igc_adapter *adapter) { (void)adapter; }
void igc_ptp_read(struct igc_adapter *adapter, struct timespec64 *ts)
{
	(void)adapter;
	*ts = ktime_to_timespec64(ktime_get_real());
}
ktime_t igc_ptp_rx_pktstamp(struct igc_adapter *adapter, __le32 *buf)
{
	(void)adapter; (void)buf;
	return 0;
}

/* Linux v6.18 `ndo_hwtstamp_get` / `ndo_hwtstamp_set` + 802.3br FPE stubs */
int igc_ptp_hwtstamp_get(struct net_device *netdev,
			 struct kernel_hwtstamp_config *config)
{
	(void)netdev; (void)config;
	return -EOPNOTSUPP;
}
int igc_ptp_hwtstamp_set(struct net_device *netdev,
			 struct kernel_hwtstamp_config *config,
			 struct netlink_ext_ack *extack)
{
	(void)netdev; (void)config; (void)extack;
	return -EOPNOTSUPP;
}
void igc_fpe_init(struct igc_adapter *adapter) { (void)adapter; }
void igc_fpe_clear_preempt_queue(struct igc_adapter *adapter) { (void)adapter; }
void igc_fpe_save_preempt_queue(struct igc_adapter *adapter,
				const struct tc_mqprio_qopt_offload *mqprio)
{
	(void)adapter; (void)mqprio;
}
u32 igc_fpe_get_supported_frag_size(u32 frag_size) { return frag_size; }

/* === v6.12 -> v6.18 PORTING DELTA: Linux v6.12 ioctl timestamp stubs === */
int igc_ptp_get_ts_config(struct net_device *netdev, struct ifreq *ifr)
{
	(void)netdev; (void)ifr;
	return -EOPNOTSUPP;
}
int igc_ptp_set_ts_config(struct net_device *netdev, struct ifreq *ifr)
{
	(void)netdev; (void)ifr;
	return -EOPNOTSUPP;
}

/* Pruned TSN offload stubs (`feature_prune`) */
int igc_tsn_offload_apply(struct igc_adapter *adapter) { (void)adapter; return 0; }
int igc_tsn_reset(struct igc_adapter *adapter) { (void)adapter; return 0; }
void igc_tsn_adjust_txtime_offset(struct igc_adapter *adapter) { (void)adapter; }
bool igc_tsn_is_taprio_activated_by_user(struct igc_adapter *adapter)
{
	(void)adapter;
	return false;
}

/* Unsupported in-kernel eBPF / AF_XDP stubs (`unsupported` -> `-EOPNOTSUPP`) */
int igc_xdp_set_prog(struct igc_adapter *adapter, struct bpf_prog *prog,
		     struct netlink_ext_ack *extack)
{
	(void)adapter; (void)prog;
	NL_SET_ERR_MSG_MOD(extack, "In-kernel eBPF XDP unsupported in DRY C Driver Island");
	return -EOPNOTSUPP;
}
int igc_xdp_setup_pool(struct igc_adapter *adapter, struct xsk_buff_pool *pool,
		       u16 queue_id)
{
	(void)adapter; (void)pool; (void)queue_id;
	return -EOPNOTSUPP;
}
