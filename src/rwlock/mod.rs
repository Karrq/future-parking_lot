// Copyright 2018 Marco Napetti
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use std::task::Waker;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::ptr::null_mut;
use crossbeam_queue::SegQueue;

/// FutureRead module
pub mod read;
/// FutureUpgradableRead module
pub mod upgradable_read;
/// FutureWrite module
pub mod write;

/// Trait to permit FutureRead implementation on wrapped RwLock (not RwLock itself)
pub use read::FutureReadable;
/// Trait to permit FutureUpgradableRead implementation on wrapped RwLock (not RwLock itself)
pub use upgradable_read::FutureUpgradableReadable;
/// Trait to permit FutureWrite implementation on wrapped RwLock (not RwLock itself)
pub use write::FutureWriteable;

use lock_api::{RwLock as RwLock_, RawRwLock};

use parking_lot::RawRwLock as RawRwLock_;

/// a Future-compatible parking_lot::RwLock
pub type RwLock<T> = RwLock_<FutureRawRwLock<RawRwLock_>, T>;

/// RawRwLock implementor that collects Wakers to wake them up when unlocked
pub struct FutureRawRwLock<R: RawRwLock> {
    locking: AtomicBool,
    wakers: AtomicPtr<SegQueue<Waker>>,
    inner: R,
}

impl<R> FutureRawRwLock<R> where R: RawRwLock {
    // this is needed to avoid sequences like that:
    // * thread 1 gains lock
    // * thread 2 try lock
    // * thread 1 unlock
    // * thread 2 register waker
    // this creates a situation similar to a deadlock, where the future isn't waked up by nobody
    fn atomic_lock(&self) {
        while self.locking.compare_and_swap(false, true, Ordering::Relaxed) {}
    }

    fn atomic_unlock(&self) {
        self.locking.store(false, Ordering::Relaxed);
    }

    fn register_waker(&self, waker: &Waker) {
        let v = unsafe { &mut *self.wakers.load(Ordering::Relaxed) };
        v.push(waker.clone());
        // implicitly unlock
        self.atomic_unlock();
    }

    fn create_wakers_list(&self) {
        let v = self.wakers.load(Ordering::Relaxed);
        if v.is_null() {
            let temp = Box::new(SegQueue::new());
            self.wakers.compare_and_swap(v, Box::into_raw(temp), Ordering::Relaxed);
        }
    }

    fn wake_up(&self) {
        self.atomic_lock();
        let v = unsafe { &mut *self.wakers.load(Ordering::Relaxed) };
        if let Ok(w) = v.pop() {
            w.wake();
        }
        self.atomic_unlock();
    }
}

impl<R> Drop for FutureRawRwLock<R> where R: RawRwLock {
    fn drop(&mut self) {
        let v = self.wakers.load(Ordering::Relaxed);
        if !v.is_null() {
            unsafe { Box::from_raw(v) };
        }
    }
}

unsafe impl<R> RawRwLock for FutureRawRwLock<R> where R: RawRwLock {
    type GuardMarker = R::GuardMarker;

    const INIT: FutureRawRwLock<R> = {
        FutureRawRwLock {
            locking: AtomicBool::new(false),
            wakers: AtomicPtr::new(null_mut()),
            inner: R::INIT
        }
    };

    fn lock_shared(&self) {
        self.create_wakers_list();

        self.inner.lock_shared();
    }

    fn try_lock_shared(&self) -> bool {
        self.create_wakers_list();

        self.inner.try_lock_shared()
    }

    fn unlock_shared(&self) {
        self.inner.unlock_shared();

        self.wake_up();
    }

    fn lock_exclusive(&self) {
        self.create_wakers_list();

        self.inner.lock_exclusive();
    }

    fn try_lock_exclusive(&self) -> bool {
        self.create_wakers_list();

        self.inner.try_lock_exclusive()
    }

    fn unlock_exclusive(&self) {
        self.inner.unlock_exclusive();

        self.wake_up();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::rc::Rc;

    use tokio::runtime::Runtime as ThreadpoolRuntime;
    use tokio::runtime::current_thread::Runtime as CurrentThreadRuntime;

    use super::{RwLock, FutureReadable, FutureWriteable};

    use lazy_static::lazy_static;

    use log::info;

    lazy_static! {
        static ref LOCK1: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
        static ref LOCK2: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
        static ref CONCURRENT_LOCK: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
    }

    #[test]
    fn current_thread_lazy_static() {
        env_logger::try_init().ok();

        let mut runtime = CurrentThreadRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = LOCK1.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = LOCK1.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn current_thread_local_arc() {
        env_logger::try_init().ok();

        let lock = Arc::new(RwLock::new(Vec::new()));
        let mut runtime = CurrentThreadRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = lock.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = lock.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn current_thread_local_rc() {
        env_logger::try_init().ok();

        let lock = Rc::new(RwLock::new(Vec::new()));
        let mut runtime = CurrentThreadRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = lock.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = lock.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn current_thread_local_box() {
        env_logger::try_init().ok();

        let lock = Box::new(RwLock::new(Vec::new()));
        let mut runtime = CurrentThreadRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = lock.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = lock.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn multithread_lazy_static() {
        env_logger::try_init().ok();

        let runtime = ThreadpoolRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = LOCK2.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = LOCK2.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn multithread_local_arc() {
        env_logger::try_init().ok();

        let lock = Arc::new(RwLock::new(Vec::new()));
        let runtime = ThreadpoolRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = lock.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = lock.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn multithread_local_rc() {
        env_logger::try_init().ok();

        let lock = Rc::new(RwLock::new(Vec::new()));
        let runtime = ThreadpoolRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = lock.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = lock.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn multithread_local_box() {
        env_logger::try_init().ok();

        let lock = Box::new(RwLock::new(Vec::new()));
        let runtime = ThreadpoolRuntime::new().unwrap();
        runtime.block_on(async {
            {
                let mut v = lock.future_write().await;
                v.push(String::from("It works!"));
            }

            let v = lock.future_read().await;
            assert!(v.len() == 1 && v[0] == "It works!");
        });
    }

    #[test]
    fn multithread_concurrent_lazy_static() {
        env_logger::try_init().ok();

        let runtime = ThreadpoolRuntime::new().unwrap();
        runtime.block_on(async {
            // spawn 10 concurrent futures
            for i in 0..100 {
                tokio::spawn(async move {
                    {
                        let mut v = CONCURRENT_LOCK.future_write().await;
                        v.push(i.to_string());
                    }

                    let v = CONCURRENT_LOCK.future_read().await;
                    info!("{}, pushed {}", v.len(), i);
                });
            }
        });
        runtime.shutdown_on_idle();
        let singleton = CONCURRENT_LOCK.read();
        assert_eq!(singleton.len(), 100);
    }
}
