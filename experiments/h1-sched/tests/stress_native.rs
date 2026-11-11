#![cfg(not(loom))]
//! Native Multi-Core SMP Stress & Priority Work-Stealing Verification for DRY Kernel H1 Scheduler.
//!
//! Runs both:
//!  1. Hardware-matched 8-CPU stress test (matching host 4C/8T Tiger Lake)
//!  2. 20-logical-CPU oversubscribed target topology test (matching DRY OS 20-CPU architecture goal)
//!
//! Executes >= 10,000,000 total state transitions across sleep/wakeup, preemption, cross-CPU
//! work-stealing, priority ordering, cancellation, and termination, verifying:
//!  - 0 dual-CPU kernel stack collisions (`stack_owner` & monotonic stack frame checks)
//!  - 0 lost wakeups (every signaled notification is observed without timeout/deadlock)
//!  - 0 duplicate runqueue dispatches or orphaned tasks

use dry_h1_sched::{
    CommitParkOutcome, ExecState, Notification, PerCpuRunQueue, Task, WakeOutcome,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn unit_priority_round_robin_and_steal_order() {
    let rq0 = PerCpuRunQueue::<4>::new(16);
    // Enqueue low priority (3), medium (1), high (0), and another high (0)
    assert!(rq0.push(3, 30));
    assert!(rq0.push(1, 10));
    assert!(rq0.push(0, 1));
    assert!(rq0.push(0, 2));

    // Local pop serves highest priority (0) in FIFO round-robin order first
    assert_eq!(rq0.pop_local(), Some(1));
    // Cross-CPU steal also steals highest available READY task
    assert_eq!(rq0.steal(), Some(2));
    assert_eq!(rq0.pop_local(), Some(10));
    assert_eq!(rq0.steal(), Some(30));
    assert_eq!(rq0.pop_local(), None);
}

#[test]
fn stress_native_8cpu_over_10m_transitions_zero_races() {
    run_smp_stress_cluster(8, 24, 12_000_000);
}

#[test]
fn stress_native_20cpu_dryos_target_topology() {
    run_smp_stress_cluster(20, 40, 12_000_000);
}

fn run_smp_stress_cluster(num_cpus: usize, num_tasks: usize, target_transitions: u64) {
    let tasks: Arc<Vec<Task>> = Arc::new((0..num_tasks).map(Task::new_ready).collect());
    let notifs: Arc<Vec<Notification>> =
        Arc::new((0..num_tasks).map(|_| Notification::new()).collect());
    let rqs: Arc<Vec<PerCpuRunQueue<4>>> =
        Arc::new((0..num_cpus).map(|_| PerCpuRunQueue::new(64)).collect());

    // Distribute initial READY tasks across per-CPU runqueues with mixed priorities (0..4)
    for id in 0..num_tasks {
        let prio = id % 4;
        tasks[id].prepare_enqueue();
        assert!(rqs[id % num_cpus].push(prio, id));
    }

    let transitions = Arc::new(AtomicU64::new(0));
    let wakeups_delivered = Arc::new(AtomicU64::new(0));
    let steals_succeeded = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    let mut handles = Vec::with_capacity(num_cpus);
    for cpu_id in 0..num_cpus {
        let tasks = Arc::clone(&tasks);
        let notifs = Arc::clone(&notifs);
        let rqs = Arc::clone(&rqs);
        let transitions = Arc::clone(&transitions);
        let wakeups_delivered = Arc::clone(&wakeups_delivered);
        let steals_succeeded = Arc::clone(&steals_succeeded);
        let stop = Arc::clone(&stop);

        handles.push(thread::spawn(move || {
            let mut rng = (cpu_id as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut local_ops: u64 = 0;

            while !stop.load(Ordering::Relaxed) {
                // Xorshift64* PRNG for deterministic zero-dependency perturbation
                rng ^= rng >> 12;
                rng ^= rng << 25;
                rng ^= rng >> 27;
                let step = rng.wrapping_mul(0x2545_F491_4F6C_DD1D);

                // Periodically fire cross-CPU IRQ/IPC notifications + spurious wakeups on random tasks
                let target_task = (step as usize) % num_tasks;
                let prio = target_task % 4;
                let wake_cpu = ((step >> 16) as usize) % num_cpus;
                if (step & 0x3) == 0 {
                    notifs[target_task].signal(1, &tasks[target_task], rqs[wake_cpu].ring(prio), target_task);
                    wakeups_delivered.fetch_add(1, Ordering::Relaxed);
                    local_ops += 1;
                } else if (step & 0x7) == 1 {
                    // Spurious / timeout wakeup without setting notification bits
                    if tasks[target_task].wakeup() == WakeOutcome::WokeBlocked {
                        tasks[target_task].finish_waking_and_push(rqs[wake_cpu].ring(prio), target_task);
                    }
                    local_ops += 1;
                }

                // Pop from local CPU runqueue first; if empty, work-steal from peer CPUs
                let mut picked = rqs[cpu_id].pop_local();
                let mut was_stolen = false;
                if picked.is_none() {
                    for offset in 1..num_cpus {
                        let victim = (cpu_id + offset) % num_cpus;
                        if let Some(id) = rqs[victim].steal() {
                            picked = Some(id);
                            was_stolen = true;
                            break;
                        }
                    }
                }

                if let Some(task_id) = picked {
                    let task = &tasks[task_id];
                    let task_prio = task_id % 4;
                    assert!(
                        task.try_dispatch(cpu_id),
                        "READY task popped from runqueue must succeed try_dispatch when not terminated"
                    );
                    if was_stolen {
                        steals_succeeded.fetch_add(1, Ordering::Relaxed);
                    }

                    // Mutate task's non-atomic stack frame while owning stack on `cpu_id`
                    task.touch_stack_frame();
                    local_ops += 2;

                    // Choose action: preempt, cancel-park, or full sleep-park
                    match (step >> 8) & 0x3 {
                        0 | 1 => {
                            // Preempt (half the time while TOK_ARMED!)
                            if (step & 0x10) != 0 {
                                task.prepare_park();
                            }
                            assert!(task.begin_preempt(cpu_id));
                            task.post_switch_preempt(cpu_id, rqs[cpu_id].ring(task_prio), task_id);
                            local_ops += 2;
                        }
                        2 => {
                            // Voluntary prepare_park -> cancel_park -> preempt back to runqueue
                            task.prepare_park();
                            task.cancel_park();
                            assert!(task.begin_preempt(cpu_id));
                            task.post_switch_preempt(cpu_id, rqs[cpu_id].ring(task_prio), task_id);
                            local_ops += 3;
                        }
                        _ => {
                            // Full H1 park protocol
                            task.prepare_park();
                            if notifs[task_id].consume() == 0 {
                                match task.commit_park(cpu_id) {
                                    CommitParkOutcome::ConsumedToken => {
                                        assert!(task.begin_preempt(cpu_id));
                                        task.post_switch_preempt(
                                            cpu_id,
                                            rqs[cpu_id].ring(task_prio),
                                            task_id,
                                        );
                                    }
                                    CommitParkOutcome::SwitchToPark => {
                                        task.post_switch_park_id(
                                            cpu_id,
                                            rqs[cpu_id].ring(task_prio),
                                            task_id,
                                        );
                                    }
                                    CommitParkOutcome::Exiting => unreachable!(),
                                }
                            } else {
                                task.cancel_park();
                                assert!(task.begin_preempt(cpu_id));
                                task.post_switch_preempt(
                                    cpu_id,
                                    rqs[cpu_id].ring(task_prio),
                                    task_id,
                                );
                            }
                            local_ops += 3;
                        }
                    }
                } else {
                    thread::yield_now();
                }

                if local_ops >= 4096 {
                    let total = transitions.fetch_add(local_ops, Ordering::Relaxed) + local_ops;
                    local_ops = 0;
                    if total >= target_transitions {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }

            if local_ops > 0 {
                transitions.fetch_add(local_ops, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.join().expect("CPU worker thread panicked during H1 SMP stress");
    }

    // Final quiescent drain & lost-wakeup audit:
    // Signal all tasks and verify zero tasks remain stuck in `Blocked`!
    for id in 0..num_tasks {
        notifs[id].signal(1, &tasks[id], rqs[id % num_cpus].ring(id % 4), id);
        assert_ne!(
            tasks[id].state(),
            ExecState::Blocked,
            "Task {id} stuck in BLOCKED after final signal!"
        );
    }

    // Terminate and reap every task cleanly after draining runqueues
    for task in tasks.iter() {
        task.terminate();
    }
    for rq in rqs.iter() {
        while let Some(id) = rq.pop_local() {
            assert!(!tasks[id].try_dispatch(0));
            tasks[id].acknowledge_dropped_rq_entry();
        }
    }
    for (id, task) in tasks.iter().enumerate() {
        assert!(
            task.try_reap(),
            "Task {id} failed quiescent reap (state={:?}, in_flight_refs={})",
            task.state(),
            task.in_flight_refs()
        );
        assert_eq!(task.state(), ExecState::Dead);
    }

    let elapsed = start.elapsed().max(Duration::from_micros(1));
    let total_ops = transitions.load(Ordering::Relaxed);
    let total_steals = steals_succeeded.load(Ordering::Relaxed);
    let total_wakes = wakeups_delivered.load(Ordering::Relaxed);
    println!(
        "[H1-STRESS] cpus={num_cpus} tasks={num_tasks} transitions={total_ops} steals={total_steals} wakeups={total_wakes} elapsed={elapsed:.2?} rate={:.2}M ops/s",
        (total_ops as f64 / elapsed.as_secs_f64()) / 1_000_000.0
    );
    assert!(total_ops >= target_transitions);
    assert!(total_steals > 0, "Work-stealing path must execute actively under SMP stress");
}

