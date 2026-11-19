// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Explicit 7-State GOP-to-KMS Display Handover State Machine with Fail-Safe GOP Rollback
//! (`Milestone 3.4` / `5.2`).

use super::abi::GpuBackendKind;
use crate::EINVAL;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayHandoverState {
    GopActive = 0,
    Probing = 1,
    DriverReady = 2,
    ModesetArmed = 3,
    KmsActive = 4,
    FaultedRecovering = 5,
    FallbackGop = 6,
}

impl DisplayHandoverState {
    #[inline]
    pub const fn bit(self) -> u8 {
        1u8 << (self as u8)
    }

    pub const fn as_str(self) -> &'static [u8] {
        match self {
            Self::GopActive => b"GOP_ACTIVE",
            Self::Probing => b"PROBING",
            Self::DriverReady => b"DRIVER_READY",
            Self::ModesetArmed => b"MODESET_ARMED",
            Self::KmsActive => b"KMS_ACTIVE",
            Self::FaultedRecovering => b"FAULTED_RECOVERING",
            Self::FallbackGop => b"FALLBACK_GOP",
        }
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::GopActive, Self::Probing | Self::FallbackGop)
                | (Self::Probing, Self::DriverReady | Self::FaultedRecovering | Self::FallbackGop)
                | (Self::DriverReady, Self::ModesetArmed | Self::FaultedRecovering | Self::FallbackGop)
                | (Self::ModesetArmed, Self::KmsActive | Self::FaultedRecovering | Self::FallbackGop)
                | (Self::KmsActive, Self::ModesetArmed | Self::FaultedRecovering | Self::FallbackGop)
                | (Self::FaultedRecovering, Self::Probing | Self::FallbackGop)
                | (Self::FallbackGop, Self::Probing)
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DisplayHandoverMachine {
    pub state: DisplayHandoverState,
    pub active_backend: GpuBackendKind,
    pub visited_mask: u8,
    pub transitions: u32,
    pub rollbacks_to_gop: u32,
    pub kms_activations: u32,
}

impl DisplayHandoverMachine {
    pub const ALL_STATES_MASK: u8 = 0x7f; // Bits 0..=6

    pub const fn new() -> Self {
        Self {
            state: DisplayHandoverState::GopActive,
            active_backend: GpuBackendKind::UefiGop,
            visited_mask: DisplayHandoverState::GopActive.bit(),
            transitions: 0,
            rollbacks_to_gop: 0,
            kms_activations: 0,
        }
    }

    pub fn transition(&mut self, next: DisplayHandoverState) -> Result<(), isize> {
        if !self.state.can_transition_to(next) {
            return Err(EINVAL);
        }
        self.state = next;
        self.visited_mask |= next.bit();
        self.transitions = self.transitions.saturating_add(1);
        match next {
            DisplayHandoverState::FallbackGop => {
                self.active_backend = GpuBackendKind::UefiGop;
                self.rollbacks_to_gop = self.rollbacks_to_gop.saturating_add(1);
            }
            DisplayHandoverState::KmsActive => {
                self.kms_activations = self.kms_activations.saturating_add(1);
            }
            _ => {}
        }
        Ok(())
    }

    /// Execute the validated promotion sequence (`GopActive`/`FallbackGop -> Probing -> DriverReady -> ModesetArmed -> KmsActive`).
    pub fn promote_to_kms(&mut self, backend: GpuBackendKind) -> Result<(), isize> {
        self.transition(DisplayHandoverState::Probing)?;
        self.transition(DisplayHandoverState::DriverReady)?;
        self.transition(DisplayHandoverState::ModesetArmed)?;
        self.active_backend = backend;
        self.transition(DisplayHandoverState::KmsActive)
    }

    /// Fail-safe recovery rollback (`-> FaultedRecovering -> FallbackGop`).
    pub fn enter_fault_and_fallback(&mut self) {
        if self.state != DisplayHandoverState::FaultedRecovering
            && self.state.can_transition_to(DisplayHandoverState::FaultedRecovering)
        {
            let _ = self.transition(DisplayHandoverState::FaultedRecovering);
        }
        if self.state != DisplayHandoverState::FallbackGop {
            let _ = self.transition(DisplayHandoverState::FallbackGop);
        }
    }

    #[inline]
    pub const fn is_kms_active(&self) -> bool {
        matches!(self.state, DisplayHandoverState::KmsActive)
    }
}

impl Default for DisplayHandoverMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// Self-test verifying all 7 states, illegal shortcut rejection, fault fallback, and recovery.
pub fn verify_handover_invariants() -> bool {
    let mut hm = DisplayHandoverMachine::new();
    // Illegal shortcut: cannot skip directly from GopActive to KmsActive without probe/modeset.
    if hm.transition(DisplayHandoverState::KmsActive) != Err(EINVAL) {
        return false;
    }
    if hm.promote_to_kms(GpuBackendKind::VirtioGpu).is_err() {
        return false;
    }
    if !hm.is_kms_active() || hm.active_backend != GpuBackendKind::VirtioGpu {
        return false;
    }
    hm.enter_fault_and_fallback();
    if hm.state != DisplayHandoverState::FallbackGop
        || hm.active_backend != GpuBackendKind::UefiGop
        || hm.rollbacks_to_gop != 1
    {
        return false;
    }
    if hm.promote_to_kms(GpuBackendKind::LinuxDrmBochs).is_err() {
        return false;
    }
    hm.visited_mask == DisplayHandoverMachine::ALL_STATES_MASK
        && hm.kms_activations == 2
}
