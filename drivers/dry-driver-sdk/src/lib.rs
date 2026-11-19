// SPDX-License-Identifier: MIT OR Apache-2.0
//! Standalone DRY OS Driver Island SDK (`#![no_std]`).
//!
//! Provides versioned ABI v1 negotiation, bounded MMIO/DMA range validators,
//! `IoRing` framing checks, and explicit island lifecycle state tracking.

#![cfg_attr(not(test), no_std)]

pub mod abi;
pub mod mmio;
pub mod ring;

pub use abi::{
    validate_abi_hello, IslandError, IslandLifecycle, IslandState, ISLAND_ABI_VERSION_V1,
    ISLAND_FEAT_IORING, ISLAND_FEAT_MMIO_BOUNDS, ISLAND_FEAT_QUIESCE,
};
pub use mmio::{DmaRegion, MmioWindow};
pub use ring::{check_ring_header, SdkCqe, SdkSqe, SDK_IORING_MAGIC};
