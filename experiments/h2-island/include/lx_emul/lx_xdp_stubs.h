/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Unsupported Monolithic-Kernel Behavior Stubs (XDP / AF_XDP / uaccess)
 *
 * Architecturally unsupported in an isolated userspace C Driver Island:
 *   1. In-kernel eBPF / XDP / AF_XDP zero-copy kernel UMEM & JIT execution.
 *   2. Direct cross-address-space client pointer copy (`copy_from_user` / `copy_to_user`).
 * Kept as unreachable compile-time stubs (`adapter->xdp_prog == NULL`, `ring->xsk_pool == NULL`).
 */
#ifndef LX_EMUL_XDP_STUBS_H
#define LX_EMUL_XDP_STUBS_H

static inline void xdp_init_buff(struct xdp_buff *xdp, u32 frame_sz, struct xdp_rxq_info *rxq)
{
	memset(xdp, 0, sizeof(*xdp));
	xdp->frame_sz = frame_sz;
	xdp->rxq = rxq;
}
static inline void xdp_prepare_buff(struct xdp_buff *xdp, unsigned char *hard_start,
				    int headroom, int data_len, const bool meta_valid)
{
	(void)meta_valid;
	xdp->data_hard_start = hard_start;
	xdp->data = hard_start + headroom;
	xdp->data_end = (unsigned char *)xdp->data + data_len;
	xdp->data_meta = xdp->data;
}
static inline void xdp_buff_clear_frags_flag(struct xdp_buff *xdp) { xdp->flags = 0; }
static inline bool xdp_rxq_info_is_reg(struct xdp_rxq_info *rxq) { return rxq->reg; }
static inline int xdp_rxq_info_reg(struct xdp_rxq_info *rxq, struct net_device *dev,
				   u32 queue_index, unsigned int napi_id)
{
	(void)napi_id;
	rxq->dev = dev;
	rxq->queue_index = queue_index;
	rxq->reg = true;
	return 0;
}
static inline void xdp_rxq_info_unreg(struct xdp_rxq_info *rxq) { rxq->reg = false; }
static inline int xdp_rxq_info_reg_mem_model(struct xdp_rxq_info *rxq, enum xdp_mem_type type, void *alloc)
{
	(void)rxq; (void)type; (void)alloc;
	return 0;
}
static inline void xdp_rxq_info_unreg_mem_model(struct xdp_rxq_info *rxq) { (void)rxq; }
static inline u32 bpf_prog_run_xdp(const struct bpf_prog *prog, struct xdp_buff *xdp)
{
	(void)prog; (void)xdp;
	return XDP_PASS;
}
static inline void bpf_warn_invalid_xdp_action(struct net_device *dev, struct bpf_prog *prog, u32 act)
{
	(void)dev; (void)prog; (void)act;
}
static inline void trace_xdp_exception(const struct net_device *dev, const struct bpf_prog *prog, u32 act)
{
	(void)dev; (void)prog; (void)act;
}
static inline int xdp_do_redirect(struct net_device *dev, struct xdp_buff *xdp, struct bpf_prog *prog)
{
	(void)dev; (void)xdp; (void)prog;
	return -EOPNOTSUPP;
}
static inline void xdp_do_flush(void) {}
static inline struct xdp_frame *xdp_convert_buff_to_frame(struct xdp_buff *xdp) { (void)xdp; return NULL; }
static inline void xdp_return_frame(struct xdp_frame *xdpf) { (void)xdpf; }
static inline void xdp_return_frame_rx_napi(struct xdp_frame *xdpf) { (void)xdpf; }
static inline bool xdp_frame_has_frags(const struct xdp_frame *frame) { (void)frame; return false; }
static inline u32 xdp_get_frame_len(struct xdp_frame *frame) { return frame->len; }
static inline struct skb_shared_info *xdp_get_shared_info_from_frame(const struct xdp_frame *frame)
{
	(void)frame;
	return NULL;
}

static inline struct xsk_buff_pool *xsk_get_pool_from_qid(struct net_device *dev, u16 queue_id)
{
	(void)dev; (void)queue_id;
	return NULL;
}
static inline void xsk_set_rx_need_wakeup(struct xsk_buff_pool *pool) { (void)pool; }
static inline void xsk_set_tx_need_wakeup(struct xsk_buff_pool *pool) { (void)pool; }
static inline void xsk_clear_rx_need_wakeup(struct xsk_buff_pool *pool) { (void)pool; }
static inline bool xsk_uses_need_wakeup(struct xsk_buff_pool *pool) { (void)pool; return false; }
static inline u32 xsk_pool_get_rx_frame_size(struct xsk_buff_pool *pool) { (void)pool; return 2048; }
static inline int xsk_pool_set_rxq_info(struct xsk_buff_pool *pool, struct xdp_rxq_info *rxq)
{
	(void)pool; (void)rxq;
	return -EOPNOTSUPP;
}
static inline struct xdp_buff *xsk_buff_alloc(struct xsk_buff_pool *pool) { (void)pool; return NULL; }
static inline void xsk_buff_free(struct xdp_buff *xdp) { (void)xdp; }
static inline dma_addr_t xsk_buff_xdp_get_dma(struct xdp_buff *xdp) { (void)xdp; return 0; }
static inline dma_addr_t xsk_buff_raw_get_dma(struct xsk_buff_pool *pool, u64 addr) { (void)pool; (void)addr; return 0; }
static inline void xsk_buff_dma_sync_for_cpu(struct xdp_buff *xdp) { (void)xdp; }
static inline void xsk_buff_raw_dma_sync_for_device(struct xsk_buff_pool *pool, dma_addr_t dma, size_t size)
{
	(void)pool; (void)dma; (void)size;
}
static inline bool xsk_tx_peek_desc(struct xsk_buff_pool *pool, struct xdp_desc *desc)
{
	(void)pool; (void)desc;
	return false;
}
static inline void xsk_tx_release(struct xsk_buff_pool *pool) { (void)pool; }
static inline void xsk_tx_completed(struct xsk_buff_pool *pool, u32 nb_entries) { (void)pool; (void)nb_entries; }
static inline struct xsk_tx_metadata *xsk_buff_get_metadata(struct xsk_buff_pool *pool, u64 addr)
{
	(void)pool; (void)addr;
	return NULL;
}
static inline void xsk_tx_metadata_request(struct xsk_tx_metadata *meta,
					   const struct xsk_tx_metadata_ops *ops,
					   void *priv)
{
	(void)meta; (void)ops; (void)priv;
}
static inline void xsk_tx_metadata_to_compl(struct xsk_tx_metadata *meta,
					    struct xsk_tx_metadata_compl *compl)
{
	(void)meta; (void)compl;
}
static inline void xsk_tx_metadata_complete(struct xsk_tx_metadata_compl *compl,
					    const struct xsk_tx_metadata_ops *ops,
					    void *priv)
{
	(void)compl; (void)ops; (void)priv;
}

/* === v6.12 -> v6.18 PORTING DELTA (cross-address-space ioctl stubs in v6.12) === */
static inline unsigned long copy_from_user(void *to, const void __user *from, unsigned long n)
{
	(void)to; (void)from;
	return n; /* Rejected in isolated userspace Driver Island */
}
static inline unsigned long copy_to_user(void __user *to, const void *from, unsigned long n)
{
	(void)to; (void)from;
	return n; /* Rejected in isolated userspace Driver Island */
}

#endif /* LX_EMUL_XDP_STUBS_H */
