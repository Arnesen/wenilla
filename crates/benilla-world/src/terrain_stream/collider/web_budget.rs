//! Wenilla's browser construction budget. A coroutine yield is not a browser frame yield:
//! pending builds wake only when the Stream schedule grants another frame's budget.
//! Keep the original trimesh shape: movement/backface and decal consumers downcast to it.
//! A single build is still indivisible; oversized jobs are measured by the caller.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::task::{Poll, Waker};
use std::time::Duration;

const BUILD_BUDGET: Duration = Duration::from_millis(2);
type Waiter = Rc<RefCell<Option<Waker>>>;

#[cfg(target_arch = "wasm32")]
pub(super) fn build(
    verts: Vec<bevy::prelude::Vec3>,
    tris: Vec<[u32; 3]>,
) -> bevy::tasks::Task<avian3d::prelude::Collider> {
    super::AsyncComputeTaskPool::get().spawn(async move {
        wait_turn().await;
        let started = bevy::platform::time::Instant::now();
        let triangles = tris.len();
        let collider = avian3d::prelude::Collider::trimesh(verts, tris);
        let elapsed = started.elapsed();
        finish_build(elapsed);
        if elapsed > Duration::from_millis(8) {
            bevy::log::warn!(
                triangles,
                ms = elapsed.as_secs_f64() * 1000.0,
                "browser collider build exceeded 8 ms; geometry retained for one-sided collision"
            );
        }
        collider
    })
}

struct Budget {
    spent: Duration,
    queue: VecDeque<Weak<RefCell<Option<Waker>>>>,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            spent: BUILD_BUDGET,
            queue: VecDeque::new(),
        }
    }
}

impl Budget {
    fn first(&mut self) -> Option<Waiter> {
        loop {
            let first = self.queue.front()?;
            if let Some(waiter) = first.upgrade() {
                return Some(waiter);
            }
            self.queue.pop_front(); // dropped Task: never retain its geometry or waker
        }
    }

    fn poll(&mut self, waiter: &Waiter, waker: &Waker) -> Poll<()> {
        *waiter.borrow_mut() = Some(waker.clone());
        if self.spent < BUILD_BUDGET && self.first().is_some_and(|first| Rc::ptr_eq(&first, waiter))
        {
            self.queue.pop_front();
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn wake(&mut self) -> Option<Waker> {
        if self.spent >= BUILD_BUDGET {
            return None;
        }
        self.first().and_then(|waiter| waiter.borrow_mut().take())
    }
}

#[cfg(target_arch = "wasm32")]
thread_local! { static BUDGET: RefCell<Budget> = RefCell::new(Budget::default()); }

#[cfg(target_arch = "wasm32")]
pub(super) fn begin_frame() {
    let wake = BUDGET.with(|budget| {
        let mut budget = budget.borrow_mut();
        budget.spent = Duration::ZERO;
        budget.wake()
    });
    if let Some(wake) = wake {
        wake.wake();
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn wait_turn() {
    let waiter = Rc::new(RefCell::new(None));
    BUDGET.with(|budget| budget.borrow_mut().queue.push_back(Rc::downgrade(&waiter)));
    std::future::poll_fn(|cx| BUDGET.with(|budget| budget.borrow_mut().poll(&waiter, cx.waker())))
        .await;
}

#[cfg(target_arch = "wasm32")]
pub(super) fn finish_build(elapsed: Duration) {
    let wake = BUDGET.with(|budget| {
        let mut budget = budget.borrow_mut();
        budget.spent += elapsed;
        budget.wake()
    });
    if let Some(wake) = wake {
        wake.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn enqueue(budget: &mut Budget) -> Waiter {
        let waiter = Rc::new(RefCell::new(None));
        budget.queue.push_back(Rc::downgrade(&waiter));
        waiter
    }

    #[test]
    fn burst_waits_for_the_next_frame_and_keeps_fifo_order() {
        let mut budget = Budget::default();
        let a = enqueue(&mut budget);
        let b = enqueue(&mut budget);
        assert!(budget.poll(&a, Waker::noop()).is_pending());
        budget.spent = Duration::ZERO;
        assert!(budget.poll(&b, Waker::noop()).is_pending());
        assert!(budget.poll(&a, Waker::noop()).is_ready());
        budget.spent += Duration::from_millis(3); // one oversize job still makes progress
        assert!(budget.wake().is_none());
        assert!(budget.poll(&b, Waker::noop()).is_pending());
        budget.spent = Duration::ZERO;
        assert!(budget.wake().is_some());
        assert!(budget.poll(&b, Waker::noop()).is_ready());
        assert!(budget.queue.is_empty());
    }

    #[test]
    fn cancelled_job_does_not_block_survivors() {
        let mut budget = Budget::default();
        let cancelled = enqueue(&mut budget);
        let survivor = enqueue(&mut budget);
        drop(cancelled);
        budget.spent = Duration::ZERO;
        assert!(budget.poll(&survivor, Waker::noop()).is_ready());
        assert!(budget.queue.is_empty());
    }
}
