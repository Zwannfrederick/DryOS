// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Mandatory NVIDIA Driver Island (`Level 1 — DRIVER ISLAND READY`, fail-closed `BLOCKED`).
//!
//! Coordinates non-destructive prerequisite checks for NVIDIA RTX 3050 Ti Laptop (`10de:25a0`),
//! honest `nvidia-smi` / `-L` / `-q` formatting (zero fabricated telemetry), capability-isolated
//! `dry-nv-island.elf` supervision, and verified `nvidia_exp/` USB log bundle export.

pub mod prereq;
pub mod protocol;
pub mod session_logs;
pub mod smi_format;
