/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_INTERRUPT_H
#define _DRY_LINUX_INTERRUPT_H

#include <linux/compat_base.h>

typedef int irqreturn_t;

#define IRQ_NONE 0
#define IRQ_HANDLED 1
#define IRQ_WAKE_THREAD 2
#define IRQ_RETVAL(x) ((x) ? IRQ_HANDLED : IRQ_NONE)

#define IRQF_SHARED 0x00000080UL
#define IRQF_ONESHOT 0x00002000UL

typedef irqreturn_t (*irq_handler_t)(int irq, void *dev_id);

int request_irq(unsigned int irq, irq_handler_t handler, unsigned long flags,
                const char *name, void *dev);
void free_irq(unsigned int irq, void *dev_id);
void disable_irq(unsigned int irq);
void enable_irq(unsigned int irq);
int dry_lx_irq_poll(void);
u64 dry_lx_irq_handled_count(void);
bool dry_lx_irq_is_msi(void);

#endif /* _DRY_LINUX_INTERRUPT_H */
