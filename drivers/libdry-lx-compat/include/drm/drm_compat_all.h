/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_DRM_COMPAT_ALL_H
#define _DRY_DRM_COMPAT_ALL_H

#include <linux/compat_base.h>
#include <linux/pci.h>

#define DRIVER_GEM 0x01U
#define DRIVER_MODESET 0x02U
#define DRIVER_ATOMIC 0x04U

#define DRM_FORMAT_XRGB8888 0x34325258U /* 'XR24' */
#define DRM_FORMAT_BGRX8888 0x34325842U /* 'BX24' */

#define DRM_PLANE_TYPE_PRIMARY 1
#define DRM_MODE_ENCODER_VIRTUAL 5
#define DRM_MODE_CONNECTOR_VIRTUAL 15
#define DRM_PLANE_NO_SCALING (1 << 16)
#define EDID_LENGTH 128

enum drm_mode_status {
    MODE_OK = 0,
    MODE_BAD_WIDTH = -2,
    MODE_MEM = -3,
    MODE_ERROR = -1,
};

struct drm_device;
struct drm_plane;
struct drm_crtc;
struct drm_encoder;
struct drm_connector;
struct drm_atomic_state;

struct drm_format_info {
    u32 format;
    u8 cpp[4];
    u8 num_planes;
};

struct drm_framebuffer {
    const struct drm_format_info *format;
    u32 width;
    u32 height;
    u32 pitches[4];
};

struct drm_rect {
    int x1;
    int y1;
    int x2;
    int y2;
};

struct drm_clip_rect {
    unsigned short x1;
    unsigned short y1;
    unsigned short x2;
    unsigned short y2;
};

struct iosys_map {
    void __iomem *vaddr_iomem;
    void *vaddr;
    bool is_iomem;
};

#define IOSYS_MAP_INIT_VADDR_IOMEM(p) \
    ((struct iosys_map){ .vaddr_iomem = (void __iomem *)(p), .vaddr = (void *)(p), .is_iomem = true })

static inline void iosys_map_incr(struct iosys_map *map, size_t incr) {
    if (map->is_iomem) {
        map->vaddr_iomem = (void __iomem *)((uint8_t *)map->vaddr_iomem + incr);
    }
    map->vaddr = (uint8_t *)map->vaddr + incr;
}

struct drm_display_mode {
    u32 hdisplay;
    u32 vdisplay;
    int clock;
};

struct drm_crtc_state {
    struct drm_display_mode mode;
    bool enable;
};

struct drm_plane_state {
    struct drm_framebuffer *fb;
    int crtc_x;
    int crtc_y;
    struct drm_crtc *crtc;
    bool visible;
};

struct drm_shadow_plane_state {
    struct drm_plane_state base;
    struct iosys_map data[4];
};

#define to_drm_shadow_plane_state(st) container_of((st), struct drm_shadow_plane_state, base)

struct drm_scanout_buffer {
    const struct drm_format_info *format;
    unsigned int width;
    unsigned int height;
    unsigned int pitch[4];
    struct iosys_map map[4];
};

struct drm_plane_helper_funcs {
    void *begin_fb_access;
    int (*atomic_check)(struct drm_plane *plane, struct drm_atomic_state *state);
    void (*atomic_update)(struct drm_plane *plane, struct drm_atomic_state *state);
    int (*get_scanout_buffer)(struct drm_plane *plane, struct drm_scanout_buffer *sb);
};

#define DRM_GEM_SHADOW_PLANE_HELPER_FUNCS .begin_fb_access = NULL

struct drm_plane_funcs {
    void (*reset)(struct drm_plane *plane);
    int (*update_plane)(void);
    int (*disable_plane)(void);
    void (*destroy)(struct drm_plane *plane);
};

#define DRM_GEM_SHADOW_PLANE_FUNCS .reset = NULL

struct drm_plane {
    struct drm_device *dev;
    struct drm_plane_state *state;
    const struct drm_plane_helper_funcs *helper_private;
    const struct drm_plane_funcs *funcs;
};

struct drm_crtc_helper_funcs {
    void (*mode_set_nofb)(struct drm_crtc *crtc);
    int (*atomic_check)(struct drm_crtc *crtc, struct drm_atomic_state *state);
    void (*atomic_enable)(struct drm_crtc *crtc, struct drm_atomic_state *state);
    void (*atomic_disable)(struct drm_crtc *crtc, struct drm_atomic_state *state);
};

struct drm_crtc_funcs {
    void (*reset)(void);
    void (*destroy)(void);
    int (*set_config)(void);
    int (*page_flip)(void);
    void *(*atomic_duplicate_state)(void);
    void (*atomic_destroy_state)(void);
};

struct drm_crtc {
    struct drm_device *dev;
    struct drm_crtc_state *state;
    const struct drm_crtc_helper_funcs *helper_private;
    const struct drm_crtc_funcs *funcs;
    unsigned int index;
};

struct drm_encoder_funcs {
    void (*destroy)(void);
};

struct drm_encoder {
    struct drm_device *dev;
    const struct drm_encoder_funcs *funcs;
    unsigned int possible_crtcs;
};

struct drm_connector_helper_funcs {
    int (*get_modes)(struct drm_connector *connector);
};

struct drm_connector_funcs {
    int (*fill_modes)(void);
    void (*destroy)(void);
    void (*reset)(void);
    void *(*atomic_duplicate_state)(void);
    void (*atomic_destroy_state)(void);
};

struct drm_connector {
    struct drm_device *dev;
    const struct drm_connector_helper_funcs *helper_private;
    const struct drm_connector_funcs *funcs;
    int edid_valid;
    int edid_pref_w;
    int edid_pref_h;
};

struct drm_edid {
    u8 raw[128];
    int valid;
    int pref_w;
    int pref_h;
};

struct drm_mode_config_funcs {
    struct drm_framebuffer *(*fb_create)(void);
    enum drm_mode_status (*mode_valid)(struct drm_device *dev, const struct drm_display_mode *mode);
    int (*atomic_check)(void);
    int (*atomic_commit)(void);
};

struct drm_mode_config {
    int max_width;
    int max_height;
    int preferred_depth;
    bool quirk_addfb_prefer_host_byte_order;
    const struct drm_mode_config_funcs *funcs;
};

struct drm_device {
    struct device *dev;
    struct drm_mode_config mode_config;
    struct drm_plane *primary_plane;
    struct drm_crtc *crtc;
    struct drm_connector *connector;
};

struct file_operations {
    int unused;
};

struct drm_driver {
    u32 driver_features;
    const struct file_operations *fops;
    const char *name;
    const char *desc;
    int major;
    int minor;
    void *gem_prime_mmap;
    void *fbdev_probe;
};

#define DEFINE_DRM_GEM_FOPS(name) static const struct file_operations name = { 0 }
#define DRM_GEM_SHMEM_DRIVER_OPS .gem_prime_mmap = NULL
#define DRM_FBDEV_SHMEM_DRIVER_OPS .fbdev_probe = NULL

struct drm_atomic_state {
    struct drm_plane_state *new_plane_state;
    struct drm_plane_state *old_plane_state;
    struct drm_crtc_state *new_crtc_state;
};

struct drm_atomic_helper_damage_iter {
    int emitted;
    struct drm_rect clip;
};

void *dry_lx_devm_zalloc(struct device *dev, size_t size);
#define devm_drm_dev_alloc(parent, drv, type, member) ({                  \
    (void)(drv);                                                          \
    type *__d = (type *)dry_lx_devm_zalloc((parent), sizeof(type));       \
    if (__d) {                                                            \
        __d->member.dev = (parent);                                       \
    }                                                                     \
    __d ? __d : (type *)(void *)0;                                        \
})

#define drm_module_pci_driver_if_modeset(drv_sym, cond_flag)              \
    const struct pci_driver *dry_lx_registered_pci_driver(void) {         \
        (void)(cond_flag);                                                \
        return &(drv_sym);                                                \
    }

const struct drm_format_info *drm_format_info(u32 format);
u64 drm_format_info_min_pitch(const struct drm_format_info *info, int plane, unsigned int width);
size_t drm_fb_clip_offset(u32 pitch, const struct drm_format_info *format, const struct drm_rect *clip);
void drm_fb_memcpy(struct iosys_map *dst, const u32 *dst_pitch, const struct iosys_map *src,
                   const struct drm_framebuffer *fb, const struct drm_rect *clip);

#define drm_dev_enter(dev_ptr, idx_ptr) ((void)(dev_ptr), *(idx_ptr) = 0, 1)
void drm_dev_exit(int idx);
int drm_dev_register(struct drm_device *dev, unsigned long flags);
void drm_dev_put(struct drm_device *dev);
void drm_dev_unplug(struct drm_device *dev);
void drm_client_setup(struct drm_device *dev, const void *opt);

int drmm_mode_config_init(struct drm_device *dev);
void drm_mode_config_reset(struct drm_device *dev);
int drm_universal_plane_init(struct drm_device *dev, struct drm_plane *plane, uint32_t possible_crtcs,
                             const struct drm_plane_funcs *funcs, const uint32_t *formats,
                             unsigned int format_count, const uint64_t *format_modifiers,
                             int type, const char *name, ...);
void drm_plane_helper_add(struct drm_plane *plane, const struct drm_plane_helper_funcs *funcs);
void drm_plane_enable_fb_damage_clips(struct drm_plane *plane);
int drm_crtc_init_with_planes(struct drm_device *dev, struct drm_crtc *crtc, struct drm_plane *primary,
                              struct drm_plane *cursor, const struct drm_crtc_funcs *funcs,
                              const char *name, ...);
void drm_crtc_helper_add(struct drm_crtc *crtc, const struct drm_crtc_helper_funcs *funcs);
static inline unsigned int drm_crtc_mask(const struct drm_crtc *crtc) {
    return 1U << crtc->index;
}
int drm_encoder_init(struct drm_device *dev, struct drm_encoder *encoder,
                     const struct drm_encoder_funcs *funcs, int encoder_type, const char *name, ...);
int drm_connector_init(struct drm_device *dev, struct drm_connector *connector,
                       const struct drm_connector_funcs *funcs, int connector_type);
void drm_connector_helper_add(struct drm_connector *connector, const struct drm_connector_helper_funcs *funcs);
void drm_connector_attach_edid_property(struct drm_connector *connector);
int drm_connector_attach_encoder(struct drm_connector *connector, struct drm_encoder *encoder);

int drm_edid_header_is_valid(const u8 *raw_edid);
const struct drm_edid *drm_edid_read_custom(
    struct drm_connector *connector,
    int (*read_block)(void *context, u8 *buf, unsigned int block, size_t len),
    void *context);
void drm_edid_connector_update(struct drm_connector *connector, const struct drm_edid *edid);
int drm_edid_connector_add_modes(struct drm_connector *connector);
void drm_edid_free(const struct drm_edid *edid);
int drm_add_modes_noedid(struct drm_connector *connector, int maxx, int maxy);
void drm_set_preferred_mode(struct drm_connector *connector, int hpref, int vpref);

struct drm_plane_state *drm_atomic_get_new_plane_state(struct drm_atomic_state *state, struct drm_plane *plane);
struct drm_plane_state *drm_atomic_get_old_plane_state(struct drm_atomic_state *state, struct drm_plane *plane);
struct drm_crtc_state *drm_atomic_get_new_crtc_state(struct drm_atomic_state *state, struct drm_crtc *crtc);
int drm_atomic_helper_check_plane_state(struct drm_plane_state *plane_state,
                                        const struct drm_crtc_state *crtc_state,
                                        int min_scale, int max_scale, bool can_position,
                                        bool can_update_disabled);
int drm_atomic_helper_check_crtc_primary_plane(const struct drm_crtc_state *crtc_state);

void drm_atomic_helper_damage_iter_init(struct drm_atomic_helper_damage_iter *iter,
                                        const struct drm_plane_state *old_state,
                                        const struct drm_plane_state *new_state);
bool dry_lx_next_damage_rect(struct drm_atomic_helper_damage_iter *iter, struct drm_rect *rect);
#define drm_atomic_for_each_plane_damage(iter_ptr, rect_ptr) \
    while (dry_lx_next_damage_rect((iter_ptr), (rect_ptr)))

int drm_atomic_helper_update_plane(void);
int drm_atomic_helper_disable_plane(void);
void drm_plane_cleanup(struct drm_plane *plane);
void drm_atomic_helper_crtc_reset(void);
void drm_crtc_cleanup(void);
int drm_atomic_helper_set_config(void);
int drm_atomic_helper_page_flip(void);
void *drm_atomic_helper_crtc_duplicate_state(void);
void drm_atomic_helper_crtc_destroy_state(void);
void drm_encoder_cleanup(void);
int drm_helper_probe_single_connector_modes(void);
void drm_connector_cleanup(void);
void drm_atomic_helper_connector_reset(void);
void *drm_atomic_helper_connector_duplicate_state(void);
void drm_atomic_helper_connector_destroy_state(void);
struct drm_framebuffer *drm_gem_fb_create_with_dirty(void);
int drm_atomic_helper_check(void);
int drm_atomic_helper_commit(void);
void drm_atomic_helper_shutdown(struct drm_device *dev);

#endif
