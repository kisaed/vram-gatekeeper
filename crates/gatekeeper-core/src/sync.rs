//! Lock helpers that shrug off poisoning.
//!
//! Every lock in this crate guards plain data that stays consistent when a
//! holder panics, so a poisoned lock is recovered rather than propagated.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Poison-tolerant locking for [`Mutex`].
pub trait MutexExt<T> {
    /// Locks the mutex, recovering the data if a holder panicked.
    fn lock_ok(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Poison-tolerant locking for [`RwLock`].
pub trait RwLockExt<T> {
    /// Takes a read lock, recovering the data if a holder panicked.
    fn read_ok(&self) -> RwLockReadGuard<'_, T>;
    /// Takes a write lock, recovering the data if a holder panicked.
    fn write_ok(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_ok(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_ok(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}
