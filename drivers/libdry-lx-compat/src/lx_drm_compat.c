/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#include <linux/compat_base.h>
#include <linux/dma-buf.h>
#include <linux/pci.h>
#include <drm/drm_compat_all.h>
#include <dry_lx_drm.h>

extern void dry_lx_devm_reset(void);
extern const struct pci_driver *dry_lx_registered_pci_driver(void);

static struct pci_dev g_drm_pdev;
static struct drm_edid g_edid;
static struct drm_device *g_drm_dev = NULL;
static struct drm_crtc_state g_crtc_state;
static struct drm_shadow_plane_state g_old_plane_state;
static struct drm_shadow_plane_state g_new_plane_state;
static struct drm_atomic_state g_atomic_state;
static struct drm_framebuffer g_fb;
static struct drm_rect g_pending_damage;
static spinlock_t g_drm_fence_lock;
static u64 g_drm_fence_seqno = 0ULL;

static const struct drm_format_info g_fmt_xrgb8888 = {
    .format = DRM_FORMAT_XRGB8888,
    .cpp = { 4, 0, 0, 0 },
    .num_planes = 1,
};

static const struct drm_format_info g_fmt_bgrx8888 = {
    .format = DRM_FORMAT_BGRX8888,
    .cpp = { 4, 0, 0, 0 },
    .num_planes = 1,
};

const struct drm_format_info *drm_format_info(u32 format) {
    if (format == DRM_FORMAT_XRGB8888) {
        return &g_fmt_xrgb8888;
    }
    if (format == DRM_FORMAT_BGRX8888) {
        return &g_fmt_bgrx8888;
    }
    return NULL;
}

u64 drm_format_info_min_pitch(const struct drm_format_info *info, int plane, unsigned int width) {
    if (!info || plane != 0) {
        return 0;
    }
    return (u64)width * (u64)info->cpp[0];
}

size_t drm_fb_clip_offset(u32 pitch, const struct drm_format_info *format, const struct drm_rect *clip) {
    u32 cpp = (format && format->cpp[0]) ? format->cpp[0] : 4U;
    return (size_t)clip->y1 * (size_t)pitch + (size_t)clip->x1 * (size_t)cpp;
}

void drm_fb_memcpy(struct iosys_map *dst, const u32 *dst_pitch, const struct iosys_map *src,
                   const struct drm_framebuffer *fb, const struct drm_rect *clip) {
    if (!dst || !dst_pitch || !src || !fb || !clip || !src[0].vaddr) {
        return;
    }
    int x1 = clip->x1;
    int y1 = clip->y1;
    int x2 = clip->x2;
    int y2 = clip->y2;
    if (x2 <= x1 || y2 <= y1) {
        return;
    }
    u32 pitch = fb->pitches[0];
    size_t row_bytes = (size_t)(x2 - x1) * 4U;
    volatile uint8_t *d_base = (volatile uint8_t *)dst->vaddr_iomem;
    const uint8_t *s_base = (const uint8_t *)src[0].vaddr + (size_t)y1 * (size_t)pitch + (size_t)x1 * 4U;

    for (int row = 0; row < (y2 - y1); row++) {
        volatile uint32_t *dw = (volatile uint32_t *)(d_base + (size_t)row * (size_t)dst_pitch[0]);
        const uint32_t *sw = (const uint32_t *)(s_base + (size_t)row * (size_t)pitch);
        size_t dwords = row_bytes / 4U;
        for (size_t i = 0; i < dwords; i++) {
            dw[i] = sw[i];
        }
    }
}

void drm_dev_exit(int idx) { (void)idx; }

int drm_dev_register(struct drm_device *dev, unsigned long flags) {
    (void)flags;
    g_drm_dev = dev;
    return 0;
}

void drm_dev_put(struct drm_device *dev) { (void)dev; }
void drm_dev_unplug(struct drm_device *dev) { (void)dev; }
void drm_client_setup(struct drm_device *dev, const void *opt) { (void)dev; (void)opt; }

int drmm_mode_config_init(struct drm_device *dev) {
    dry_lx_memset(&dev->mode_config, 0, sizeof(dev->mode_config));
    return 0;
}

void drm_mode_config_reset(struct drm_device *dev) {
    if (dev->crtc) {
        dev->crtc->state = &g_crtc_state;
    }
    if (dev->primary_plane) {
        dev->primary_plane->state = &g_new_plane_state.base;
    }
}

int drm_universal_plane_init(struct drm_device *dev, struct drm_plane *plane, uint32_t possible_crtcs,
                             const struct drm_plane_funcs *funcs, const uint32_t *formats,
                             unsigned int format_count, const uint64_t *format_modifiers,
                             int type, const char *name, ...) {
    (void)possible_crtcs; (void)formats; (void)format_count;
    (void)format_modifiers; (void)type; (void)name;
    plane->dev = dev;
    plane->funcs = funcs;
    plane->state = &g_new_plane_state.base;
    dev->primary_plane = plane;
    return 0;
}

void drm_plane_helper_add(struct drm_plane *plane, const struct drm_plane_helper_funcs *funcs) {
    plane->helper_private = funcs;
}

void drm_plane_enable_fb_damage_clips(struct drm_plane *plane) { (void)plane; }

int drm_crtc_init_with_planes(struct drm_device *dev, struct drm_crtc *crtc, struct drm_plane *primary,
                              struct drm_plane *cursor, const struct drm_crtc_funcs *funcs,
                              const char *name, ...) {
    (void)primary; (void)cursor; (void)name;
    crtc->dev = dev;
    crtc->funcs = funcs;
    crtc->index = 0;
    crtc->state = &g_crtc_state;
    dev->crtc = crtc;
    return 0;
}

void drm_crtc_helper_add(struct drm_crtc *crtc, const struct drm_crtc_helper_funcs *funcs) {
    crtc->helper_private = funcs;
}

int drm_encoder_init(struct drm_device *dev, struct drm_encoder *encoder,
                     const struct drm_encoder_funcs *funcs, int encoder_type, const char *name, ...) {
    (void)encoder_type; (void)name;
    encoder->dev = dev;
    encoder->funcs = funcs;
    return 0;
}

int drm_connector_init(struct drm_device *dev, struct drm_connector *connector,
                       const struct drm_connector_funcs *funcs, int connector_type) {
    (void)connector_type;
    connector->dev = dev;
    connector->funcs = funcs;
    dev->connector = connector;
    return 0;
}

void drm_connector_helper_add(struct drm_connector *connector, const struct drm_connector_helper_funcs *funcs) {
    connector->helper_private = funcs;
}

void drm_connector_attach_edid_property(struct drm_connector *connector) { (void)connector; }

int drm_connector_attach_encoder(struct drm_connector *connector, struct drm_encoder *encoder) {
    (void)connector; (void)encoder;
    return 0;
}

int drm_edid_header_is_valid(const u8 *raw_edid) {
    static const u8 hdr[8] = { 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00 };
    int score = 0;
    if (!raw_edid) {
        return 0;
    }
    for (int i = 0; i < 8; i++) {
        if (raw_edid[i] == hdr[i]) {
            score++;
        }
    }
    return score;
}

const struct drm_edid *drm_edid_read_custom(
    struct drm_connector *connector,
    int (*read_block)(void *context, u8 *buf, unsigned int block, size_t len),
    void *context) {
    (void)connector;
    dry_lx_memset(&g_edid, 0, sizeof(g_edid));
    if (!read_block || read_block(context, g_edid.raw, 0, EDID_LENGTH) != 0) {
        return NULL;
    }
    if (drm_edid_header_is_valid(g_edid.raw) != 8) {
        return NULL;
    }
    int hactive = (int)g_edid.raw[56] | (((int)(g_edid.raw[58] & 0xf0U)) << 4);
    int vactive = (int)g_edid.raw[59] | (((int)(g_edid.raw[61] & 0xf0U)) << 4);
    g_edid.valid = 1;
    g_edid.pref_w = (hactive > 0) ? hactive : 1024;
    g_edid.pref_h = (vactive > 0) ? vactive : 768;
    return &g_edid;
}

void drm_edid_connector_update(struct drm_connector *connector, const struct drm_edid *edid) {
    if (!connector) {
        return;
    }
    if (edid && edid->valid) {
        connector->edid_valid = 1;
        connector->edid_pref_w = edid->pref_w;
        connector->edid_pref_h = edid->pref_h;
    } else {
        connector->edid_valid = 0;
    }
}

int drm_edid_connector_add_modes(struct drm_connector *connector) {
    return (connector && connector->edid_valid) ? 1 : 0;
}

void drm_edid_free(const struct drm_edid *edid) { (void)edid; }

int drm_add_modes_noedid(struct drm_connector *connector, int maxx, int maxy) {
    (void)connector; (void)maxx; (void)maxy;
    return 1;
}

void drm_set_preferred_mode(struct drm_connector *connector, int hpref, int vpref) {
    if (connector) {
        connector->edid_pref_w = hpref;
        connector->edid_pref_h = vpref;
    }
}

struct drm_plane_state *drm_atomic_get_new_plane_state(struct drm_atomic_state *state, struct drm_plane *plane) {
    (void)plane;
    return state->new_plane_state;
}

struct drm_plane_state *drm_atomic_get_old_plane_state(struct drm_atomic_state *state, struct drm_plane *plane) {
    (void)plane;
    return state->old_plane_state;
}

struct drm_crtc_state *drm_atomic_get_new_crtc_state(struct drm_atomic_state *state, struct drm_crtc *crtc) {
    (void)crtc;
    return state->new_crtc_state;
}

int drm_atomic_helper_check_plane_state(struct drm_plane_state *plane_state,
                                        const struct drm_crtc_state *crtc_state,
                                        int min_scale, int max_scale, bool can_position,
                                        bool can_update_disabled) {
    (void)min_scale; (void)max_scale; (void)can_position; (void)can_update_disabled;
    if (!plane_state || !crtc_state || !crtc_state->enable || !plane_state->fb) {
        return 0;
    }
    plane_state->visible = true;
    return 0;
}

int drm_atomic_helper_check_crtc_primary_plane(const struct drm_crtc_state *crtc_state) {
    (void)crtc_state;
    return 0;
}

void drm_atomic_helper_damage_iter_init(struct drm_atomic_helper_damage_iter *iter,
                                        const struct drm_plane_state *old_state,
                                        const struct drm_plane_state *new_state) {
    (void)old_state; (void)new_state;
    iter->emitted = 0;
    iter->clip = g_pending_damage;
}

bool dry_lx_next_damage_rect(struct drm_atomic_helper_damage_iter *iter, struct drm_rect *rect) {
    if (!iter || !rect || iter->emitted != 0) {
        return false;
    }
    if (iter->clip.x2 <= iter->clip.x1 || iter->clip.y2 <= iter->clip.y1) {
        return false;
    }
    *rect = iter->clip;
    iter->emitted = 1;
    return true;
}

int drm_atomic_helper_update_plane(void) { return 0; }
int drm_atomic_helper_disable_plane(void) { return 0; }
void drm_plane_cleanup(struct drm_plane *plane) { (void)plane; }
void drm_atomic_helper_crtc_reset(void) {}
void drm_crtc_cleanup(void) {}
int drm_atomic_helper_set_config(void) { return 0; }
int drm_atomic_helper_page_flip(void) { return 0; }
void *drm_atomic_helper_crtc_duplicate_state(void) { return NULL; }
void drm_atomic_helper_crtc_destroy_state(void) {}
void drm_encoder_cleanup(void) {}
int drm_helper_probe_single_connector_modes(void) { return 0; }
void drm_connector_cleanup(void) {}
void drm_atomic_helper_connector_reset(void) {}
void *drm_atomic_helper_connector_duplicate_state(void) { return NULL; }
void drm_atomic_helper_connector_destroy_state(void) {}
struct drm_framebuffer *drm_gem_fb_create_with_dirty(void) { return NULL; }
int drm_atomic_helper_check(void) { return 0; }
int drm_atomic_helper_commit(void) { return 0; }
void drm_atomic_helper_shutdown(struct drm_device *dev) { (void)dev; }

int32_t dry_lx_drm_probe_pci_device(
    uint16_t vendor_id, uint16_t device_id, uint16_t subvendor, uint16_t subdevice,
    uint16_t bdf, uint8_t revision, uint16_t dev_flags,
    const struct dry_lx_pci_bar_init *bars, struct dry_lx_drm_probe_info *out) {
    const struct pci_driver *drv = dry_lx_registered_pci_driver();
    if (!drv || !drv->probe || !bars) {
        return -ENODEV;
    }
    dry_lx_devm_reset();
    spin_lock_init(&g_drm_fence_lock);
    g_drm_dev = NULL;
    dry_lx_memset(&g_crtc_state, 0, sizeof(g_crtc_state));
    dry_lx_memset(&g_old_plane_state, 0, sizeof(g_old_plane_state));
    dry_lx_memset(&g_new_plane_state, 0, sizeof(g_new_plane_state));
    dry_lx_memset(&g_atomic_state, 0, sizeof(g_atomic_state));
    dry_lx_memset(&g_fb, 0, sizeof(g_fb));
    dry_lx_memset(&g_pending_damage, 0, sizeof(g_pending_damage));

    dry_lx_pci_init_device(
        &g_drm_pdev, vendor_id, device_id, subvendor, subdevice, bdf, revision, dev_flags, bars);

    const struct pci_device_id *matched = dry_lx_match_pci_id_table(drv->id_table, &g_drm_pdev);
    if (!matched) {
        return -ENODEV;
    }
    int rc = drv->probe(&g_drm_pdev, matched);
    if (rc != 0 || !g_drm_dev) {
        return rc != 0 ? rc : -ENODEV;
    }
    if (g_drm_dev->connector && g_drm_dev->connector->helper_private &&
        g_drm_dev->connector->helper_private->get_modes) {
        g_drm_dev->connector->helper_private->get_modes(g_drm_dev->connector);
    }

    uint32_t bar_mask = dry_lx_pci_mapped_bar_mask();
    if (out) {
        out->edid_ok = (g_drm_dev->connector && g_drm_dev->connector->edid_valid) ? 1U : 0U;
        out->pref_w = g_drm_dev->connector ? (uint32_t)g_drm_dev->connector->edid_pref_w : 1024U;
        out->pref_h = g_drm_dev->connector ? (uint32_t)g_drm_dev->connector->edid_pref_h : 768U;
        out->mapped_bar0_ok = (bar_mask & (1U << 0)) ? 1U : 0U;
        out->mapped_bar2_ok = (bar_mask & (1U << 2)) ? 1U : 0U;
    }
    return 0;
}

int32_t dry_lx_drm_modeset(uint32_t w, uint32_t h) {
    if (!g_drm_dev || !g_drm_dev->crtc || !g_drm_dev->crtc->helper_private) {
        return -ENODEV;
    }
    struct drm_display_mode mode = { .hdisplay = (int)w, .vdisplay = (int)h, .clock = 65000 };
    if (g_drm_dev->mode_config.funcs && g_drm_dev->mode_config.funcs->mode_valid) {
        enum drm_mode_status st = g_drm_dev->mode_config.funcs->mode_valid(g_drm_dev, &mode);
        if (st != MODE_OK) {
            return (int32_t)st;
        }
    }
    g_crtc_state.mode = mode;
    g_crtc_state.enable = true;
    g_drm_dev->crtc->state = &g_crtc_state;
    g_atomic_state.new_crtc_state = &g_crtc_state;
    g_atomic_state.new_plane_state = &g_new_plane_state.base;
    g_atomic_state.old_plane_state = &g_old_plane_state.base;

    if (g_drm_dev->crtc->helper_private->atomic_check) {
        int chk = g_drm_dev->crtc->helper_private->atomic_check(g_drm_dev->crtc, &g_atomic_state);
        if (chk != 0) return chk;
    }
    if (g_drm_dev->crtc->helper_private->mode_set_nofb) {
        g_drm_dev->crtc->helper_private->mode_set_nofb(g_drm_dev->crtc);
    }
    if (g_drm_dev->crtc->helper_private->atomic_enable) {
        g_drm_dev->crtc->helper_private->atomic_enable(g_drm_dev->crtc, &g_atomic_state);
    }
    return 0;
}

int32_t dry_lx_drm_commit_dmabuf(
    const void *src_ptr, uint32_t w, uint32_t h, uint32_t pitch,
    int32_t x, int32_t y, int32_t dw, int32_t dh) {
    if (!g_drm_dev || !g_drm_dev->primary_plane || !g_drm_dev->primary_plane->helper_private || !src_ptr) {
        return -EINVAL;
    }
    if (w == 0U || h == 0U || pitch < w * 4U || dw <= 0 || dh <= 0) {
        return -EINVAL;
    }
    int32_t x1 = x < 0 ? 0 : x;
    int32_t y1 = y < 0 ? 0 : y;
    int32_t x2 = (x + dw) > (int32_t)w ? (int32_t)w : (x + dw);
    int32_t y2 = (y + dh) > (int32_t)h ? (int32_t)h : (y + dh);
    if (x2 <= x1 || y2 <= y1) {
        return -EINVAL;
    }

    struct dma_buf *db = NULL;
    struct dma_fence fence;
    dma_fence_init(&fence, NULL, &g_drm_fence_lock, 1ULL, ++g_drm_fence_seqno);
    if (dry_lx_dmabuf_wrap_framecap(1ULL, (void *)src_ptr, (size_t)pitch * (size_t)h,
                                    DRY_DMABUF_FLAG_RO_EXPORT, &db) != 0 ||
        !dry_lx_dmabuf_is_ro(db)) {
        return -EINVAL;
    }
    (void)dry_lx_dmabuf_attach_fence(db, &fence);

    g_fb.format = &g_fmt_xrgb8888;
    g_fb.width = w;
    g_fb.height = h;
    g_fb.pitches[0] = pitch;

    g_new_plane_state.base.fb = &g_fb;
    g_new_plane_state.base.crtc = g_drm_dev->crtc;
    g_new_plane_state.base.visible = true;
    g_new_plane_state.data[0].vaddr = dry_lx_dmabuf_vaddr(db);
    g_new_plane_state.data[0].is_iomem = false;

    g_drm_dev->primary_plane->state = &g_new_plane_state.base;
    g_pending_damage.x1 = x1;
    g_pending_damage.y1 = y1;
    g_pending_damage.x2 = x2;
    g_pending_damage.y2 = y2;

    g_atomic_state.new_crtc_state = &g_crtc_state;
    g_atomic_state.new_plane_state = &g_new_plane_state.base;
    g_atomic_state.old_plane_state = &g_old_plane_state.base;

    const struct drm_plane_helper_funcs *pf = g_drm_dev->primary_plane->helper_private;
    int rc = 0;
    if (pf->atomic_check) {
        rc = pf->atomic_check(g_drm_dev->primary_plane, &g_atomic_state);
    }
    if (rc == 0 && pf->atomic_update) {
        pf->atomic_update(g_drm_dev->primary_plane, &g_atomic_state);
    }
    dma_fence_signal(&fence);
    dma_buf_put(db);
    return rc;
}

void dry_lx_drm_shutdown(void) {
    const struct pci_driver *drv = dry_lx_registered_pci_driver();
    if (drv && drv->shutdown && g_drm_dev) {
        drv->shutdown(&g_drm_pdev);
    }
}
