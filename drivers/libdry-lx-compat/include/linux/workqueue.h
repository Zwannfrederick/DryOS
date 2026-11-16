/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_WORKQUEUE_H
#define _DRY_LINUX_WORKQUEUE_H

#include <linux/jiffies.h>

struct work_struct;
typedef void (*work_func_t)(struct work_struct *work);

struct work_struct {
    work_func_t func;
    bool pending;
};

struct delayed_work {
    struct work_struct work;
    unsigned long due_jiffies;
    bool timer_pending;
};

struct workqueue_struct {
    const char *name;
    bool active;
};

struct napi_struct;

struct workqueue_struct *alloc_workqueue(const char *fmt, unsigned int flags, int max_active);
void destroy_workqueue(struct workqueue_struct *wq);
void INIT_WORK(struct work_struct *work, work_func_t func);
void INIT_DELAYED_WORK(struct delayed_work *dwork, work_func_t func);
bool queue_work(struct workqueue_struct *wq, struct work_struct *work);
bool queue_delayed_work(struct workqueue_struct *wq, struct delayed_work *dwork, unsigned long delay);
bool cancel_delayed_work_sync(struct delayed_work *dwork);
void flush_workqueue(struct workqueue_struct *wq);

int dry_lx_wq_register_napi(struct napi_struct *napi);
void dry_lx_wq_unregister_napi(struct napi_struct *napi);
int dry_lx_wq_run_tick(int budget);

#endif /* _DRY_LINUX_WORKQUEUE_H */
