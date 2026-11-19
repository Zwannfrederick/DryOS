// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view for `dry-sysmon` (`DRY Life Station`).

use crate::action::{compartment_short_name, APP_COUNT};
use crate::apps::PixelAppWindow;
use crate::gfx::Rect;
use crate::hwdiag::{mode_slug, persist_slug, DiagSummaryWire};
use crate::shell::LineBuilder;
use crate::ttf::{font_metrics, measure_text, FontId};
use crate::ui::app_icons::{draw_app_icon, AppIconKind};
use crate::ui::containers::{metric_card, status_badge, table_header};
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::layout::{h_cell, inset_rect, split_bot, split_top};
use crate::ui::theme::{StatusKind, UiTheme};
use crate::{UserPowerInfo, UserProcInfo, UserSysInfo};

pub const SYSMON_PIXEL_VADDR: u64 = 0x1c00_0000;
pub const SYSMON_WIN_W: u16 = 560;
pub const SYSMON_WIN_H: u16 = 352;

pub struct SysmonView<'a> {
    pub sys: &'a UserSysInfo,
    pub d_sys: u64,
    pub d_ipc: u64,
    pub pwr: &'a UserPowerInfo,
    pub procs: &'a [UserProcInfo],
    pub running_apps: u64,
    pub refresh_seq: u64,
    pub lease_epoch: u64,
    pub keys_rcvd: u64,
    pub last_ascii: u8,
    pub spark_samples: &'a [u16],
    pub spark_max: u16,
    pub diag: DiagSummaryWire,
}

pub fn render_sysmon_pixel(win: &mut PixelAppWindow, view: &SysmonView<'_>) {
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let head_h = (t.ctrl_h).clamp(28, 34);
    let foot_h = 26usize;
    let (head_r, rest1) = split_top(Rect::new(0, 0, w, h), head_h, 0);
    let (body_outer, foot_r) = split_bot(rest1, foot_h, 0);
    let body_r = inset_rect(body_outer, t.sp_sm + 2, t.sp_sm);

    let kpi_h = if body_r.h >= 260 { 94usize } else { 90usize };
    let spark_h = 44usize;
    let (kpi_r, below_kpi) = split_top(body_r, kpi_h, t.sp_sm);
    let (spark_r, tbl_r) = split_top(below_kpi, spark_h, t.sp_sm);

    let mut c = win.canvas();
    c.clear(t.bg_app);

    // Header strip
    c.fill_rect(head_r, t.bg_sidebar);
    c.hline(head_r.x, head_r.y + head_r.h.saturating_sub(1), head_r.w, t.border_subtle, 220);
    let cap_m = font_metrics(FontId::SansSemiBold, t.caption_px);
    let h_ty = head_r.y as i32 + ((head_r.h as i32 - (cap_m.ascent + cap_m.descent)) / 2);
    draw_app_icon(
        &mut c,
        Rect::new(
            head_r.x + 10,
            head_r.y + (head_r.h.saturating_sub(18)) / 2,
            18,
            18,
        ),
        AppIconKind::LifeStation,
        t.mode,
    );
    c.draw_text(
        (head_r.x + 32) as i32,
        h_ty,
        FontId::SansSemiBold,
        t.caption_px,
        b"DRY Life Station",
        t.fg_primary,
    );

    let mut app_lb = LineBuilder::new();
    app_lb.push_u64(view.running_apps);
    app_lb.push_byte(b'/');
    app_lb.push_u64(APP_COUNT as u64);
    app_lb.push_bytes(b" APPS");

    let mut proc_lb = LineBuilder::new();
    proc_lb.push_u64(view.procs.len() as u64);
    proc_lb.push_bytes(b" PROCS");

    let mut seq_lb = LineBuilder::new();
    seq_lb.push_bytes(b"LIVE #");
    seq_lb.push_u64(view.refresh_seq);

    let by = head_r.y + (head_r.h.saturating_sub(20)) / 2;
    let aw = measure_text(FontId::SansSemiBold, t.caption_px, app_lb.as_bytes()).max(0) as usize + 22;
    let pw = measure_text(FontId::SansSemiBold, t.caption_px, proc_lb.as_bytes()).max(0) as usize + 22;
    let sw = measure_text(FontId::SansSemiBold, t.caption_px, seq_lb.as_bytes()).max(0) as usize + 22;

    let ax = w.saturating_sub(10 + aw);
    status_badge(&mut c, t, ax, by, app_lb.as_bytes(), StatusKind::Neutral);
    if ax > pw + 180 {
        let px = ax - pw - 6;
        status_badge(&mut c, t, px, by, proc_lb.as_bytes(), StatusKind::Accent);
        if px > sw + 180 {
            status_badge(&mut c, t, px - sw - 6, by, seq_lb.as_bytes(), StatusKind::Ok);
        }
    }

    // 3 KPI Metric Cards
    let apic_str: &[u8] = match view.sys.apic_mode {
        1 => b"x2APIC",
        2 => b"xAPIC",
        _ => b"Off",
    };
    let mut cpu_v = LineBuilder::new();
    cpu_v.push_u64(view.sys.online_cpus as u64);
    cpu_v.push_bytes(b" vCPU ");
    cpu_v.push_bytes(apic_str);
    let mut cpu_s = LineBuilder::new();
    cpu_s.push_u64(view.sys.tsc_mhz);
    cpu_s.push_bytes(b"MHz | Up ");
    cpu_s.push_u64(view.sys.uptime_ms / 1000);
    cpu_s.push_byte(b'.');
    cpu_s.push_u64_pad3(view.sys.uptime_ms % 1000);
    cpu_s.push_byte(b's');
    let cpu_card_r = h_cell(kpi_r, 3, 0, t.sp_sm);
    metric_card(
        &mut c,
        t,
        cpu_card_r,
        b"CPU & APIC",
        cpu_v.as_bytes(),
        cpu_s.as_bytes(),
        StatusKind::Ok,
        None,
    );
    if (measure_text(FontId::SansSemiBold, t.caption_px, b"CPU & APIC").max(0) as usize + 42) <= cpu_card_r.w {
        draw_vector_icon(
            &mut c,
            Rect::new(cpu_card_r.x + cpu_card_r.w.saturating_sub(24), cpu_card_r.y + 8, 14, 14),
            IconKind::Cpu,
            t.ok_fg,
            t.accent_fg,
        );
    }

    let usable = view.sys.usable_frames_4k.max(1) as u64;
    let free = (view.sys.free_frames_4k as u64).min(usable);
    let used = usable.saturating_sub(free);
    let used_pct = ((used * 100) / usable).min(100);
    let used_mib = (used * 4) / 1024;
    let usable_mib = (usable * 4) / 1024;
    let mut mem_v = LineBuilder::new();
    mem_v.push_u64(used_mib);
    mem_v.push_byte(b'/');
    mem_v.push_u64(usable_mib);
    mem_v.push_bytes(b" MiB");
    let mut mem_s = LineBuilder::new();
    mem_s.push_u64(free);
    mem_s.push_bytes(b" free (");
    mem_s.push_u64(used_pct);
    mem_s.push_bytes(b"%)");
    let mem_kind = if used_pct >= 80 { StatusKind::Warn } else { StatusKind::Ok };
    let mem_card_r = h_cell(kpi_r, 3, 1, t.sp_sm);
    metric_card(
        &mut c,
        t,
        mem_card_r,
        b"PMM MEMORY",
        mem_v.as_bytes(),
        mem_s.as_bytes(),
        mem_kind,
        Some(used_pct as u8),
    );
    if (measure_text(FontId::SansSemiBold, t.caption_px, b"PMM MEMORY").max(0) as usize + 42) <= mem_card_r.w {
        draw_vector_icon(
            &mut c,
            Rect::new(mem_card_r.x + mem_card_r.w.saturating_sub(24), mem_card_r.y + 8, 14, 14),
            IconKind::Memory,
            t.fg_secondary,
            t.accent_fg,
        );
    }

    let mut sys_v = LineBuilder::new();
    sys_v.push_byte(b'+');
    sys_v.push_u64(view.d_sys);
    sys_v.push_bytes(b" sys");
    let mut sys_s = LineBuilder::new();
    sys_s.push_bytes(b"IPC +");
    sys_s.push_u64(view.d_ipc);
    sys_s.push_bytes(b" | HLT ");
    sys_s.push_u64(view.pwr.idle_hlt_calls);
    let sys_card_r = h_cell(kpi_r, 3, 2, t.sp_sm);
    metric_card(
        &mut c,
        t,
        sys_card_r,
        b"KERNEL TRAFFIC",
        sys_v.as_bytes(),
        sys_s.as_bytes(),
        StatusKind::Accent,
        None,
    );
    if (measure_text(FontId::SansSemiBold, t.caption_px, b"KERNEL TRAFFIC").max(0) as usize + 42) <= sys_card_r.w {
        draw_vector_icon(
            &mut c,
            Rect::new(sys_card_r.x + sys_card_r.w.saturating_sub(24), sys_card_r.y + 8, 14, 14),
            IconKind::DriverIsland,
            t.accent_fg,
            t.accent_fg,
        );
    }

    // AA Syscall Delta Sparkline Card
    c.fill_bordered_rrect_aa(spark_r, t.r_md, t.bg_card, t.border_subtle);
    let sp_in = inset_rect(spark_r, 12, 6);
    c.draw_text(
        sp_in.x as i32,
        sp_in.y as i32,
        FontId::SansSemiBold,
        t.caption_px,
        b"SYSCALLS / TICK",
        t.fg_muted,
    );
    let mut tot_lb = LineBuilder::new();
    tot_lb.push_bytes(b"tot ");
    tot_lb.push_u64(view.sys.total_syscalls);
    tot_lb.push_bytes(b" | ctx ");
    tot_lb.push_u64(view.sys.total_ctx_switches);
    c.draw_text_ellipsis(
        sp_in.x as i32,
        (sp_in.y + sp_in.h.saturating_sub(15)) as i32,
        156,
        FontId::MonoRegular,
        t.caption_px,
        tot_lb.as_bytes(),
        t.fg_secondary,
    );
    let chart_x = sp_in.x + 164;
    if sp_in.w > 170 {
        let chart_r = Rect::new(chart_x, sp_in.y + 1, sp_in.w - 164, sp_in.h.saturating_sub(2));
        c.draw_sparkline_aa(
            chart_r,
            view.spark_samples,
            view.spark_max.max(8),
            t.accent_fg,
            t.accent_subtle,
            115,
        );
    }

    // Active Compartments Table
    render_proc_table(&mut c, t, tbl_r, view.procs);

    // Footer status strip
    c.fill_rect(foot_r, t.bg_sidebar);
    c.hline(foot_r.x, foot_r.y, foot_r.w, t.border_subtle, 220);
    let fy = foot_r.y as i32 + ((foot_r.h as i32 - (cap_m.ascent + cap_m.descent)) / 2);

    let mut flb = LineBuilder::new();
    flb.push_bytes(b"HW:");
    flb.push_bytes(mode_slug(view.diag.mode));
    flb.push_bytes(b" P");
    flb.push_u64(view.diag.pass_count as u64);
    flb.push_bytes(b"/F");
    flb.push_u64(view.diag.fail_count as u64);
    flb.push_bytes(b" USB:");
    flb.push_bytes(persist_slug(view.diag.persist_status));
    flb.push_bytes(b" E");
    flb.push_u64(view.lease_epoch);
    flb.push_bytes(b" K");
    flb.push_u64(view.keys_rcvd);
    if (0x21..=0x7E).contains(&view.last_ascii) {
        flb.push_bytes(b" '");
        flb.push_byte(view.last_ascii);
        flb.push_byte(b'\'');
    }
    let hint = b"[R] Refresh  [T] Tile  [X] #UD Fault  [Q] Close";
    let hw = measure_text(FontId::SansRegular, t.caption_px, hint).max(0) as usize;
    let hx = w.saturating_sub(10 + hw);
    let rem = hx.saturating_sub(18);
    if rem > 40 {
        c.draw_text_ellipsis(
            10,
            fy,
            rem as i32,
            FontId::MonoRegular,
            t.caption_px,
            flb.as_bytes(),
            if view.keys_rcvd > 0 { t.warn_fg } else { t.fg_secondary },
        );
    }
    c.draw_text_ellipsis(
        hx.max(10) as i32,
        fy,
        w.saturating_sub(hx.max(10) + 10) as i32,
        FontId::SansRegular,
        t.caption_px,
        hint,
        t.fg_muted,
    );
}

fn render_proc_table(
    c: &mut crate::ui::canvas::Canvas,
    t: UiTheme,
    tbl_r: Rect,
    procs: &[UserProcInfo],
) {
    c.fill_bordered_rrect_aa(tbl_r, t.r_md, t.bg_card, t.border_subtle);
    if tbl_r.h < 38 || tbl_r.w < 120 {
        return;
    }

    let pid_w = 42usize;
    let dom_w = 54usize;
    let st_w = 52usize;
    let pg_w = 60usize;
    let sys_w = 88usize;
    let fixed_w = pid_w + dom_w + st_w + pg_w + sys_w + 20;
    let comp_w = tbl_r.w.saturating_sub(fixed_w).max(60);

    let th_h = 22usize;
    let th_r = Rect::new(tbl_r.x + 2, tbl_r.y + 2, tbl_r.w - 4, th_h);
    let cols: [(&[u8], usize); 6] = [
        (b"PID", pid_w),
        (b"DOM", dom_w),
        (b"COMPARTMENT", comp_w),
        (b"ST", st_w),
        (b"4K PG", pg_w),
        (b"SYSCALLS", sys_w),
    ];
    table_header(c, t, th_r, &cols);

    let body_r = Rect::new(
        tbl_r.x + 4,
        tbl_r.y + th_h + 4,
        tbl_r.w.saturating_sub(8),
        tbl_r.h.saturating_sub(th_h + 8),
    );
    let row_h = (font_metrics(FontId::MonoRegular, t.caption_px).line_height as usize + 5).max(17);
    let vis_rows = (body_r.h / row_h).min(procs.len());
    let mut sc = c.sub_clip(body_r);

    let m = font_metrics(FontId::MonoRegular, t.caption_px);
    for (idx, p) in procs.iter().take(vis_rows).enumerate() {
        let ry = body_r.y + idx * row_h;
        let row_rect = Rect::new(body_r.x + 2, ry, body_r.w.saturating_sub(4), row_h.saturating_sub(1));
        if (idx & 1) == 1 {
            sc.fill_rrect_aa(row_rect, t.r_sm, t.bg_card_alt, 115);
        }
        let ty = ry as i32 + ((row_h as i32 - (m.ascent + m.descent)) / 2);
        let cx0 = body_r.x + 8;

        let mut pid_lb = LineBuilder::new();
        pid_lb.push_u64(p.pid as u64);
        sc.draw_text(cx0 as i32, ty, FontId::MonoRegular, t.caption_px, pid_lb.as_bytes(), t.fg_secondary);

        let is_root = p.domain == 1;
        let dom_x = cx0 + pid_w;
        sc.draw_text(
            dom_x as i32,
            ty,
            FontId::SansSemiBold,
            t.caption_px,
            if is_root { b"ROOT" } else { b"USER" },
            if is_root { t.accent_fg } else { t.ok_fg },
        );

        let comp_x = dom_x + dom_w;
        let cname = compartment_short_name(p.compartment_id, p.domain);
        sc.draw_text_ellipsis(
            comp_x as i32,
            ty,
            comp_w.saturating_sub(6) as i32,
            FontId::SansSemiBold,
            t.caption_px,
            cname,
            t.fg_primary,
        );

        let st_x = comp_x + comp_w;
        let (st_lbl, st_rgb): (&[u8], u32) = match p.state {
            2 => (b"RUN", t.ok_fg),
            3 => (b"EXIT", t.warn_fg),
            4 => (b"FLT!", t.danger_fg),
            5 => (b"KILL", t.danger_fg),
            _ => (b"INIT", t.fg_muted),
        };
        sc.draw_text(st_x as i32, ty, FontId::SansSemiBold, t.caption_px, st_lbl, st_rgb);

        let pg_x = st_x + st_w;
        let mut pg_lb = LineBuilder::new();
        pg_lb.push_u64((p.mapped_pages_4k + p.pt_frames_4k) as u64);
        sc.draw_text_ellipsis(
            pg_x as i32,
            ty,
            pg_w.saturating_sub(4) as i32,
            FontId::MonoRegular,
            t.caption_px,
            pg_lb.as_bytes(),
            t.fg_secondary,
        );

        let sc_x = pg_x + pg_w;
        let mut sc_lb = LineBuilder::new();
        sc_lb.push_u64(p.syscall_entries);
        sc.draw_text_ellipsis(
            sc_x as i32,
            ty,
            sys_w.saturating_sub(6) as i32,
            FontId::MonoRegular,
            t.caption_px,
            sc_lb.as_bytes(),
            t.fg_primary,
        );
    }
}
