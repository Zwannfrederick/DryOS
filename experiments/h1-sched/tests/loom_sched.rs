#![cfg(loom)]
//! Exhaustive C11 weak-memory model-checking suite (Loom 0.7) for DRY Kernel H1 Scheduler.
//!
//! Includes BOTH:
//!  1. Negative (`#[should_panic]`) adversarial tests proving Loom deterministically catches:
//!     - Missing `SeqCst` fence (`StoreLoad` reordering -> lost wakeup on weak memory / AArch64)
//!     - Blind store in `post_switch` clobbering concurrent `TOK_NOTIFIED`
//!     - Reversed publication order (`rq.push` before `EXEC_READY`) orphaning stolen tasks
//!  2. Positive exhaustive verification of the corrected H1 protocol across sleep/wake,
//!     preemption + cross-CPU work-stealing with `TOK_ARMED`, concurrent dual wakeups during
//!     `EXEC_WAKING`, voluntary cancellation, and asynchronous task termination (`EXEC_EXITING` -> `EXEC_DEAD`).

use dry_h1_sched::{
    CommitParkOutcome, ExecState, Notification, ParkToken, RunQueue, Task, WakeOutcome,
};
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::sync::Arc;
use loom::thread;
use std::sync::atomic::{AtomicUsize as HostUsize, Ordering as HostOrdering};

/// NEGATIVE TEST 1 (Expected Failure / `#[should_panic]`):
/// Proves that omitting `fence(Ordering::SeqCst)` between `prepare_park` and condition load
/// (or between condition store and `wakeup`) allows C11 / AArch64 `StoreLoad` reordering,
/// causing a deterministic lost wakeup even when `Task::sched` uses `AcqRel` CAS!
#[test]
#[should_panic(expected = "LOST WAKEUP")]
fn loom_catches_missing_seqcst_fence_storeload_lost_wakeup() {
    loom::model(|| {
        let task = Arc::new(Task::new_running_for_test(0));
        let cond = Arc::new(AtomicBool::new(false));
        let rq = Arc::new(RunQueue::new(8));

        let t_waiter = Arc::clone(&task);
        let c_waiter = Arc::clone(&cond);
        let rq_waiter = Arc::clone(&rq);

        let waiter = thread::spawn(move || {
            t_waiter.prepare_park_relaxed_no_fence_for_bug_demo();
            // BUG: No `fence(Ordering::SeqCst)` before reading `cond`!
            if !c_waiter.load(Ordering::Acquire) {
                if t_waiter.commit_park(0) == CommitParkOutcome::SwitchToPark {
                    t_waiter.post_switch_park(0, &rq_waiter);
                }
            } else {
                t_waiter.cancel_park();
            }
        });

        // Producer stores `cond = true` WITHOUT `SeqCst` fence before `wakeup`.
        cond.store(true, Ordering::Release);
        if task.wakeup() == WakeOutcome::WokeBlocked {
            task.finish_waking_and_push(&rq, 0);
        }

        waiter.join().unwrap();

        // After `cond = true`, `wakeup()`, and waiter completion, task MUST NOT rest in `Blocked`!
        assert_ne!(
            task.state(),
            ExecState::Blocked,
            "LOST WAKEUP: StoreLoad reordering left task permanently BLOCKED"
        );
    });
}

/// NEGATIVE TEST 2 (Expected Failure / `#[should_panic]`):
/// Proves that a naive blind store `(EXEC_BLOCKED, TOK_NONE)` in `post_switch` overwrites
/// a `TOK_NOTIFIED` token deposited during `EXEC_SW_PARK`.
#[test]
#[should_panic(expected = "LOST WAKEUP")]
fn loom_catches_post_switch_blind_store_lost_wakeup() {
    loom::model(|| {
        let task = Arc::new(Task::new_running_for_test(0));
        let cond = Arc::new( Notification::new() );
        let rq = Arc::new(RunQueue::new(8));

        let t_waker = Arc::clone(&task);
        let c_waker = Arc::clone(&cond);
        let rq_waker = Arc::clone(&rq);

        let waker = thread::spawn(move || {
            c_waker.signal(1, &t_waker, &rq_waker, 0);
        });

        task.prepare_park();
        if cond.poll() == 0 {
            if task.commit_park(0) == CommitParkOutcome::SwitchToPark {
                // BUG: Blindly transitions SW_PARK -> BLOCKED without checking TOK_NOTIFIED!
                task.buggy_post_switch_park_blind_store(0);
            }
        } else {
            task.cancel_park();
        }

        waker.join().unwrap();

        assert_ne!(
            task.state(),
            ExecState::Blocked,
            "LOST WAKEUP: post_switch blind store clobbered concurrent TOK_NOTIFIED"
        );
    });
}

/// NEGATIVE TEST 3 (Expected Failure / `#[should_panic]`):
/// Proves that pushing a task to `RunQueue` BEFORE transitioning `exec_state -> Ready`
/// allows a work-stealing CPU to pop the task while still `SwPreempt`, fail `try_dispatch`,
/// and leave the task permanently orphaned in `Ready` with an empty `RunQueue`.
#[test]
#[should_panic(expected = "ORPHANED READY TASK")]
fn loom_catches_push_before_ready_orphan_race() {
    loom::model(|| {
        let task = Arc::new(Task::new_running_for_test(0));
        let rq = Arc::new(RunQueue::new(8));

        let t_stealer = Arc::clone(&task);
        let rq_stealer = Arc::clone(&rq);

        // CPU 0 begins preemption -> SwPreempt
        assert!(task.begin_preempt(0));

        // CPU 1 tries to steal from CPU 0's runqueue
        let stealer = thread::spawn(move || {
            if let Some(id) = rq_stealer.steal() {
                assert_eq!(id, 0);
                if t_stealer.try_dispatch(1) {
                    t_stealer.touch_stack_frame();
                    assert!(t_stealer.begin_preempt(1));
                    t_stealer.post_switch_preempt(1, &rq_stealer, 0);
                }
            }
        });

        // BUG: CPU 0 pushes task into `rq` BEFORE marking `exec_state = Ready`!
        task.buggy_post_switch_push_before_ready(0, &rq, 0);

        stealer.join().unwrap();

        if task.state() == ExecState::Ready {
            assert!(
                !rq.is_empty(),
                "ORPHANED READY TASK: task is EXEC_READY but missing from all runqueues"
            );
        }
    });
}

/// NEGATIVE TEST 4 (Expected Failure / `#[should_panic]`):
/// Proves that publishing `Ready` to `RunQueue` BEFORE switching off the outgoing task's stack
/// causes a concurrent dual-CPU stack collision caught by `stack_owner` / `loom::cell::UnsafeCell`.
#[test]
#[should_panic]
fn loom_catches_early_ready_stack_collision() {
    loom::model(|| {
        let task = Arc::new(Task::new_running_for_test(0));
        let rq = Arc::new(RunQueue::new(8));

        let t_stealer = Arc::clone(&task);
        let rq_stealer = Arc::clone(&rq);
        let stealer = thread::spawn(move || {
            if let Some(id) = rq_stealer.steal() {
                assert_eq!(id, 0);
                if t_stealer.try_dispatch(1) {
                    t_stealer.touch_stack_frame();
                }
            }
        });

        task.buggy_preempt_early_ready_before_stack_release(0, &rq, 0);
        stealer.join().unwrap();
    });
}

/// NEGATIVE TEST 5 (Expected Failure / `#[should_panic]`):
/// Proves that a blind store `(Ready, None)` in `Waking -> Ready` resurrects an `Exiting` task.
#[test]
#[should_panic(expected = "DEAD TASK RESURRECTED")]
fn loom_catches_waking_blind_store_resurrecting_exiting_task() {
    loom::model(|| {
        let task = Arc::new(Task::new_running_for_test(0));
        let rq = Arc::new(RunQueue::new(8));

        task.prepare_park();
        assert_eq!(task.commit_park(0), CommitParkOutcome::SwitchToPark);
        task.post_switch_park(0, &rq);

        let t_killer = Arc::clone(&task);
        let killer = thread::spawn(move || {
            t_killer.terminate();
        });

        if task.wakeup() == WakeOutcome::WokeBlocked {
            thread::yield_now();
            task.buggy_finish_waking_blind_store(&rq, 0);
        }

        killer.join().unwrap();
        assert_eq!(
            task.state(),
            ExecState::Exiting,
            "DEAD TASK RESURRECTED: WAKING blind store overwrote EXEC_EXITING with EXEC_READY"
        );
    });
}

/// POSITIVE TEST 1:
/// Corrected sleep/wakeup protocol with `SeqCst` fences and `CAS`-based `post_switch_park`
/// never loses a wakeup across all C11 weak-memory interleavings.
#[test]
fn loom_sleep_wakeup_no_lost_wakeup() {
    let perms = std::sync::Arc::new(HostUsize::new(0));
    let p = std::sync::Arc::clone(&perms);
    loom::model(move || {
        p.fetch_add(1, HostOrdering::Relaxed);
        let task = Arc::new(Task::new_running_for_test(0));
        let notif = Arc::new(Notification::new());
        let rq = Arc::new(RunQueue::new(8));

        let t_waker = Arc::clone(&task);
        let n_waker = Arc::clone(&notif);
        let rq_waker = Arc::clone(&rq);

        let waker = thread::spawn(move || {
            n_waker.signal(0x1, &t_waker, &rq_waker, 0);
        });

        task.touch_stack_frame();
        task.prepare_park();
        if notif.consume() == 0 {
            match task.commit_park(0) {
                CommitParkOutcome::ConsumedToken => {
                    // Waker notified us before commit_park switched out!
                    assert_eq!(task.state(), ExecState::Running);
                    assert_ne!(notif.consume(), 0);
                }
                CommitParkOutcome::SwitchToPark => {
                    task.post_switch_park(0, &rq);
                }
                CommitParkOutcome::Exiting => unreachable!(),
            }
        } else {
            task.cancel_park();
        }

        waker.join().unwrap();

        // After waker has signaled, task MUST NEVER rest in Blocked!
        assert_ne!(task.state(), ExecState::Blocked);

        // If task switched out and became Ready in rq, CPU 1 can safely steal and run it!
        if task.state() == ExecState::Ready {
            let stolen = rq.steal().expect("READY task must be present in runqueue");
            assert_eq!(stolen, 0);
            assert!(task.try_dispatch(1));
            task.touch_stack_frame();
            assert_ne!(notif.consume(), 0);
        }
    });
    println!(
        "[LOOM] loom_sleep_wakeup_no_lost_wakeup: {} permutations",
        perms.load(HostOrdering::Relaxed)
    );
}

/// POSITIVE TEST 2:
/// Preemption + cross-CPU work-stealing while `park_token == TOK_ARMED` racing with `Notification::signal`.
/// Verifies BOTH C11 stack data-race freedom (`loom::cell::UnsafeCell`) AND `TOK_NOTIFIED` preservation!
#[test]
fn loom_preempt_and_steal_preserves_armed_token_and_stack() {
    let perms = std::sync::Arc::new(HostUsize::new(0));
    let p = std::sync::Arc::clone(&perms);
    loom::model(move || {
        p.fetch_add(1, HostOrdering::Relaxed);
        let task = Arc::new(Task::new_running_for_test(0));
        let notif = Arc::new(Notification::new());
        let rq0 = Arc::new(RunQueue::new(8));
        let rq1 = Arc::new(RunQueue::new(8));

        // Task on CPU 0 arms park_token and checks notif (initially 0)
        task.touch_stack_frame();
        task.prepare_park();
        assert_eq!(notif.poll(), 0);

        // Timer interrupt preempts task on CPU 0 right before `commit_park`!
        assert!(task.begin_preempt(0));

        let t_waker = Arc::clone(&task);
        let n_waker = Arc::clone(&notif);
        let rq1_waker = Arc::clone(&rq1);
        let waker = thread::spawn(move || {
            n_waker.signal(0x42, &t_waker, &rq1_waker, 0);
        });

        let t_cpu1 = Arc::clone(&task);
        let rq0_steal = Arc::clone(&rq0);
        let rq1_local = Arc::clone(&rq1);
        let cpu1 = thread::spawn(move || {
            if let Some(id) = rq0_steal.steal() {
                assert_eq!(id, 0);
                assert!(t_cpu1.try_dispatch(1));
                // Touch non-atomic kernel stack frame on CPU 1 — Loom panics if happens-before broke!
                t_cpu1.touch_stack_frame();
                // Resume right at `commit_park` on CPU 1!
                if t_cpu1.commit_park(1) == CommitParkOutcome::SwitchToPark {
                    t_cpu1.post_switch_park(1, &rq1_local);
                }
                true
            } else {
                false
            }
        });

        // CPU 0 finishes context switch and publishes task to rq0
        task.post_switch_preempt(0, &rq0, 0);

        waker.join().unwrap();
        let cpu1_stole = cpu1.join().unwrap();

        // Regardless of interleaving, task CANNOT rest in Blocked after waker signaled!
        assert_ne!(task.state(), ExecState::Blocked);

        if !cpu1_stole {
            // CPU 1 missed the steal; task is still at `commit_park` PC in rq0 with Notified token!
            assert_eq!(rq0.pop(), Some(0));
            assert!(task.try_dispatch(1));
            task.touch_stack_frame();
            assert_eq!(task.commit_park(1), CommitParkOutcome::ConsumedToken);
        } else if task.state() == ExecState::Ready {
            // CPU 1 stole, slept in commit_park, and waker woke task onto rq1 (post-commit_park PC).
            assert_eq!(rq1.pop(), Some(0));
            assert!(task.try_dispatch(0));
            task.touch_stack_frame();
        } else {
            // CPU 1 stole and consumed ParkToken::Notified via commit_park fast path on CPU 1.
            assert_eq!(task.state(), ExecState::Running);
        }
        assert_eq!(notif.consume(), 0x42);
    });
    println!(
        "[LOOM] loom_preempt_and_steal_preserves_armed_token_and_stack: {} permutations",
        perms.load(HostOrdering::Relaxed)
    );
}

/// POSITIVE TEST 3:
/// Concurrent dual wakeups (spurious/timeout waker + real notification waker) while `Blocked` / `Waking`.
/// Verifies at-most-once runqueue insertion (`QueueHandleConservation`) and `TOK_NOTIFIED` preservation.
#[test]
fn loom_concurrent_dual_wakeup_during_waking() {
    let perms = std::sync::Arc::new(HostUsize::new(0));
    let p = std::sync::Arc::clone(&perms);
    loom::model(move || {
        p.fetch_add(1, HostOrdering::Relaxed);
        let task = Arc::new(Task::new_running_for_test(0));
        let rq = Arc::new(RunQueue::new(8));

        // Park task cleanly into BLOCKED
        task.prepare_park();
        assert_eq!(task.commit_park(0), CommitParkOutcome::SwitchToPark);
        task.post_switch_park(0, &rq);
        assert_eq!(task.state(), ExecState::Blocked);

        let t1 = Arc::clone(&task);
        let rq1 = Arc::clone(&rq);
        let w1 = thread::spawn(move || {
            if t1.wakeup() == WakeOutcome::WokeBlocked {
                t1.finish_waking_and_push(&rq1, 0);
            }
        });

        let t2 = Arc::clone(&task);
        let rq2 = Arc::clone(&rq);
        let w2 = thread::spawn(move || {
            if t2.wakeup() == WakeOutcome::WokeBlocked {
                t2.finish_waking_and_push(&rq2, 0);
            }
        });

        w1.join().unwrap();
        w2.join().unwrap();

        assert_eq!(task.state(), ExecState::Ready);
        // Task MUST appear in `rq` ONCE and ONLY ONCE!
        assert_eq!(rq.pop(), Some(0));
        assert_eq!(rq.pop(), None, "DUPLICATE ENQUEUE: task pushed twice by concurrent wakers");

        // If one waker entered WAKING while the second arrived concurrently during WAKING,
        // `ParkToken::Notified` is preserved (never wiped by a blind store).
        assert!(matches!(task.token(), ParkToken::None | ParkToken::Notified));
    });
    println!(
        "[LOOM] loom_concurrent_dual_wakeup_during_waking: {} permutations",
        perms.load(HostOrdering::Relaxed)
    );
}

/// POSITIVE TEST 4:
/// Voluntary cancellation (`cancel_park`) racing with concurrent `wakeup()` and subsequent re-park.
#[test]
fn loom_cancel_park_races_with_wakeup() {
    let perms = std::sync::Arc::new(HostUsize::new(0));
    let p = std::sync::Arc::clone(&perms);
    loom::model(move || {
        p.fetch_add(1, HostOrdering::Relaxed);
        let task = Arc::new(Task::new_running_for_test(0));
        let notif = Arc::new(Notification::new());
        let rq = Arc::new(RunQueue::new(8));

        let t_waker = Arc::clone(&task);
        let n_waker = Arc::clone(&notif);
        let rq_waker = Arc::clone(&rq);
        let waker = thread::spawn(move || {
            n_waker.signal(0x7, &t_waker, &rq_waker, 0);
        });

        // Attempt 1: arm, then self-cancel (e.g. non-blocking poll / timeout before commit)
        task.prepare_park();
        task.cancel_park();
        assert_eq!(task.token(), ParkToken::None);

        // Attempt 2: full sleep protocol on the same Notification
        task.prepare_park();
        if notif.poll() == 0 {
            if task.commit_park(0) == CommitParkOutcome::SwitchToPark {
                task.post_switch_park(0, &rq);
            }
        } else {
            task.cancel_park();
        }

        waker.join().unwrap();
        assert_ne!(task.state(), ExecState::Blocked);
    });
    println!(
        "[LOOM] loom_cancel_park_races_with_wakeup: {} permutations",
        perms.load(HostOrdering::Relaxed)
    );
}

/// POSITIVE TEST 5:
/// Asynchronous task termination (`terminate` -> `Exiting` -> `try_reap` -> `Dead`)
/// racing concurrently against `wakeup()` during `Blocked`/`Waking` and `post_switch`.
#[test]
fn loom_termination_races_with_wakeup_and_reap() {
    let perms = std::sync::Arc::new(HostUsize::new(0));
    let p = std::sync::Arc::clone(&perms);
    loom::model(move || {
        p.fetch_add(1, HostOrdering::Relaxed);
        let task = Arc::new(Task::new_running_for_test(0));
        let rq = Arc::new(RunQueue::new(8));

        task.prepare_park();
        assert_eq!(task.commit_park(0), CommitParkOutcome::SwitchToPark);

        let t_waker = Arc::clone(&task);
        let rq_waker = Arc::clone(&rq);
        let waker = thread::spawn(move || {
            if t_waker.wakeup() == WakeOutcome::WokeBlocked {
                t_waker.finish_waking_and_push(&rq_waker, 0);
            }
        });

        let t_killer = Arc::clone(&task);
        let killer = thread::spawn(move || {
            t_killer.terminate();
        });

        task.post_switch_park(0, &rq);

        waker.join().unwrap();
        killer.join().unwrap();

        // Drain any stale runqueue handle (quiescent drain before TCB reclamation)
        while let Some(id) = rq.pop() {
            assert_eq!(id, 0);
            assert!(
                !task.try_dispatch(1),
                "TERMINATED TASK DISPATCHED: Exiting task must never enter Running"
            );
            task.acknowledge_dropped_rq_entry();
        }

        // Task must never have been resurrected out of Exiting, and must cleanly reap to Dead!
        assert_eq!(task.state(), ExecState::Exiting);
        assert!(task.try_reap(), "Quiescent Exiting task with 0 refs must reap to Dead");
        assert_eq!(task.state(), ExecState::Dead);
    });
    println!(
        "[LOOM] loom_termination_races_with_wakeup_and_reap: {} permutations",
        perms.load(HostOrdering::Relaxed)
    );
}

