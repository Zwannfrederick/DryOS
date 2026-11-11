//! Synchronization primitive shim switching seamlessly between `std` and `loom 0.7`.

#[cfg(loom)]
pub use loom::cell::UnsafeCell;
#[cfg(loom)]
pub use loom::sync::atomic::{fence, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(loom)]
pub use loom::thread::yield_now;

#[cfg(not(loom))]
pub use std::sync::atomic::{fence, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(not(loom))]
pub use std::thread::yield_now;

#[cfg(not(loom))]
#[derive(Debug)]
pub struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    pub const fn new(val: T) -> Self {
        Self(std::cell::UnsafeCell::new(val))
    }

    #[inline]
    pub fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    #[inline]
    pub fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}
