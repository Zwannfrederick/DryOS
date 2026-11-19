//! `dry-nvme` — Strictly Read-Only Ring-3 (`CPL=3`, `SecDomain::RootSystem`, `0x000b`) NVMe Service.
//!
//! Safety Invariant (`INV-DISK-SAFE-01`): Never issues write, format, trim, or dataset-management
//! commands; only Admin Identify / Create IO Queues and NVM `READ(0x02)` for GPT inspection.

#![no_std]
#![no_main]

use core::arch::asm;

use dry_userspace::nvme_ro::{NvmeReadOnlyCtrl, NVME_FLAG_GPT_OK, NVME_FLAG_IO_READ_OK};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_proto::sys_ipc_reply_extra;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_exit, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_write_debug, UserIpcRecvExt, BADGE_SUPERVISOR, EPERM, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, NVME_OP_QUERY_INFO,
};

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(srv_ep_cap: u64, pci_nvme_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || srv_ep_cap == 0 || pci_nvme_cap == 0 {
        sys_exit(191);
    }

    let mut ctrl = match NvmeReadOnlyCtrl::init(pci_nvme_cap) {
        Ok(c) => c,
        Err(e) => {
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-NVME] Read-only NVMe init failed neg_errno=");
            lb.push_i64(e as i64);
            log_line(&lb);
            sys_exit(192);
        }
    };

    {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-NVME] Read-Only controller ready bdf=");
        let bdf = ctrl.info.bus_dev_fn;
        lb.push_u64((bdf >> 8) as u64);
        lb.push_byte(b':');
        lb.push_u64(((bdf >> 3) & 0x1f) as u64);
        lb.push_byte(b'.');
        lb.push_u64((bdf & 7) as u64);
        lb.push_bytes(b" sectors=");
        lb.push_u64(ctrl.info.nsze_sectors);
        lb.push_bytes(b" lba=");
        lb.push_u64(ctrl.info.lba_bytes as u64);
        lb.push_bytes(b" io_read=");
        lb.push_bytes(if (ctrl.info.state_flags & NVME_FLAG_IO_READ_OK) != 0 {
            b"true"
        } else {
            b"false"
        });
        lb.push_bytes(b" gpt=");
        lb.push_bytes(if (ctrl.info.state_flags & NVME_FLAG_GPT_OK) != 0 {
            b"true"
        } else {
            b"false"
        });
        log_line(&lb);
    }

    loop {
        let mut ext = UserIpcRecvExt::default();
        let Ok((tag, tok, _mrs)) = sys_ipc_recv_ext(srv_ep_cap, 0, 0, &mut ext) else {
            break;
        };
        dry_userspace::abi_ext::revoke_unused_cap(ext.transferred_cap);
        let label = msg_tag_label(tag);
        if ext.sender_badge != BADGE_SUPERVISOR {
            if tok != 0 {
                let _ = sys_ipc_reply_inline(
                    srv_ep_cap,
                    tok,
                    pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                    [EPERM as u64, 0, 0, 0],
                );
            }
            continue;
        }
        match label {
            NVME_OP_QUERY_INFO => {
                if tok != 0 {
                    let bdf_vid_did = ((ctrl.info.bus_dev_fn as u64) << 32)
                        | ((ctrl.info.vendor_id as u64) << 16)
                        | (ctrl.info.device_id as u64);
                    let mrs = [
                        bdf_vid_did,
                        ctrl.info.nsze_sectors,
                        u64::from(ctrl.info.lba_bytes),
                        u64::from(ctrl.info.state_flags),
                    ];
                    let _ = sys_ipc_reply_extra(
                        srv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        mrs,
                        ctrl.info.as_bytes(),
                    );
                }
            }
            IPC_OP_EXIT_CLEAN => {
                ctrl.quiesce();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            IPC_OP_CRASH_UD2 => unsafe {
                asm!("ud2", options(nomem, nostack));
            },
            _ => {
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_ERR_NOSYS, 0, 1, 0),
                        [IPC_OP_ERR_INVAL as u64, 0, 0, 0],
                    );
                }
            }
        }
    }

    ctrl.quiesce();
    sys_exit(0);
}
