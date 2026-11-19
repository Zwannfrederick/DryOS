// SPDX-License-Identifier: MIT OR Apache-2.0
//! Driver Island ABI v1 negotiation and lifecycle state machine.

pub const ISLAND_ABI_VERSION_V1: u64 = 0x0001_0000;
pub const ISLAND_FEAT_IORING: u64 = 1 << 0;
pub const ISLAND_FEAT_QUIESCE: u64 = 1 << 1;
pub const ISLAND_FEAT_MMIO_BOUNDS: u64 = 1 << 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandError {
    AbiVersionMismatch,
    BadStateTransition,
    OutOfBounds,
    BadAlignment,
    BadWidth,
    ZeroLength,
    BadRingMagic,
    RingCorrupt,
}

impl IslandError {
    #[inline]
    pub const fn errno(self) -> isize {
        match self {
            Self::AbiVersionMismatch
            | Self::BadAlignment
            | Self::BadWidth
            | Self::ZeroLength => -22, // EINVAL
            Self::BadStateTransition => -16, // EBUSY
            Self::OutOfBounds => -14,        // EFAULT
            Self::BadRingMagic | Self::RingCorrupt => -5, // EIO
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandState {
    Stopped,
    Running,
    Quiescing,
    Faulted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IslandLifecycle {
    state: IslandState,
    generation: u64,
}

impl Default for IslandLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl IslandLifecycle {
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: IslandState::Stopped,
            generation: 0,
        }
    }

    #[inline]
    pub const fn state(&self) -> IslandState {
        self.state
    }

    #[inline]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn on_start(&mut self, generation: u64) -> Result<(), IslandError> {
        if generation == 0 || !matches!(self.state, IslandState::Stopped | IslandState::Faulted) {
            return Err(IslandError::BadStateTransition);
        }
        self.state = IslandState::Running;
        self.generation = generation;
        Ok(())
    }

    pub fn on_quiesce(&mut self) -> Result<(), IslandError> {
        if self.state != IslandState::Running {
            return Err(IslandError::BadStateTransition);
        }
        self.state = IslandState::Quiescing;
        Ok(())
    }

    pub fn on_unload(&mut self) -> Result<(), IslandError> {
        if !matches!(self.state, IslandState::Running | IslandState::Quiescing) {
            return Err(IslandError::BadStateTransition);
        }
        self.state = IslandState::Stopped;
        Ok(())
    }

    #[inline]
    pub fn on_fault(&mut self) {
        self.state = IslandState::Faulted;
    }
}

/// Validate `ISLAND_OP_HELLO` version and build the 4-word `IPC_OP_PONG` payload.
#[inline]
pub const fn validate_abi_hello(
    requested_ver: u64,
    active_gen: u64,
    features: u64,
) -> Result<[u64; 4], IslandError> {
    if requested_ver != ISLAND_ABI_VERSION_V1 {
        return Err(IslandError::AbiVersionMismatch);
    }
    Ok([ISLAND_ABI_VERSION_V1, active_gen, features, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_hello_accepts_v1_and_rejects_mismatch() {
        let feats = ISLAND_FEAT_IORING | ISLAND_FEAT_QUIESCE;
        assert_eq!(
            validate_abi_hello(ISLAND_ABI_VERSION_V1, 2, feats),
            Ok([ISLAND_ABI_VERSION_V1, 2, feats, 0])
        );
        assert_eq!(
            validate_abi_hello(0x0002_0000, 2, feats),
            Err(IslandError::AbiVersionMismatch)
        );
        assert_eq!(
            validate_abi_hello(0, 1, feats),
            Err(IslandError::AbiVersionMismatch)
        );
    }

    #[test]
    fn lifecycle_transitions_enforce_valid_states() {
        let mut lc = IslandLifecycle::new();
        assert_eq!(lc.state(), IslandState::Stopped);
        assert_eq!(lc.on_start(0), Err(IslandError::BadStateTransition));
        assert_eq!(lc.on_quiesce(), Err(IslandError::BadStateTransition));
        assert_eq!(lc.on_start(1), Ok(()));
        assert_eq!(lc.generation(), 1);
        assert_eq!(lc.on_start(2), Err(IslandError::BadStateTransition));
        assert_eq!(lc.on_quiesce(), Ok(()));
        assert_eq!(lc.state(), IslandState::Quiescing);
        assert_eq!(lc.on_unload(), Ok(()));
        assert_eq!(lc.state(), IslandState::Stopped);
        lc.on_fault();
        assert_eq!(lc.state(), IslandState::Faulted);
        assert_eq!(lc.on_start(2), Ok(()));
    }
}
