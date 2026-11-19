//! The pool: which tasks may start, on a simulated clock. No process
//! runs here; the tests play the executor, saying when each task ends.

use legume_mcp::benchmark::executor::{round, Ended, Executor};
use legume_mcp::benchmark::key::splitmix64;
use legume_mcp::benchmark::plan::Task;
use legume_mcp::benchmark::pool::{Acct, Budget, Launch, Live, Outcome, Pool, State, Want};

/// A task of `stage` (its depth) and `method`, rep `rep`, booking
/// `threads` and `mem_gb`, with no upstream.
fn want(stage: usize, method: Option<&str>, rep: u32, threads: u32, mem_gb: f64) -> Want {
    Want {
        id: format!("t{stage}.{}.r{rep}", method.unwrap_or("-")),
        stage,
        stage_name: format!("S{stage}"),
        method: method.map(str::to_string),
        rep,
        threads,
        mem_gb,
        gpus: 0,
        max_running: None,
        upstream: Vec::new(),
    }
}

fn budget(cores: u32, mem_gb: f64) -> Budget {
    Budget {
        cores,
        mem_gb,
        gpus: Vec::new(),
        gpu_slots: 1,
    }
}

/// `budget(8, 100.0)` with GPU devices `gpus`, `slots` tasks each.
fn gpu_budget(gpus: Vec<u32>, slots: u32) -> Budget {
    Budget {
        gpus,
        gpu_slots: slots,
        ..budget(8, 100.0)
    }
}

/// `gb` of memory available, or no reading, and disk enough.
fn mem(gb: Option<f64>) -> Live {
    Live {
        mem_available_gb: gb,
        disk_low: false,
    }
}

/// Plenty of free memory and disk.
fn calm() -> Live {
    mem(Some(1e6))
}

fn tasks(l: &[Launch]) -> Vec<usize> {
    l.iter().map(|l| l.task).collect()
}

/// A deterministic generator, so the properties replay.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 += 1;
        splitmix64(self.0) % n
    }
}

#[test]
fn launches_what_fits_and_backfills() {
    // 4 cores: the 3-thread task starts, the second 3-thread one does not
    // fit, and the 1-thread task behind it backfills.
    let w = vec![
        want(0, None, 0, 3, 1.0),
        want(0, None, 1, 3, 1.0),
        want(0, None, 2, 1, 1.0),
    ];
    let mut p = Pool::new(w, budget(4, 100.0)).unwrap();
    assert_eq!(tasks(&p.next(0, &calm())), vec![0, 2]);
    assert_eq!(p.booked(), 4);
    assert!(p.next(0, &calm()).is_empty());
}

#[test]
fn deepest_stage_first_then_rep_major() {
    let mut w = vec![
        want(0, None, 0, 1, 1.0),
        want(0, None, 1, 1, 1.0),
        want(1, None, 1, 1, 1.0),
        want(1, None, 0, 1, 1.0),
    ];
    w[2].upstream = vec![1];
    w[3].upstream = vec![0];
    let mut p = Pool::new(w, budget(1, 100.0)).unwrap();
    assert_eq!(tasks(&p.next(0, &calm())), vec![0]);
    p.ended(10, 0, Outcome::Done).unwrap();
    // Stage 1 rep 0 is ready now and outranks stage 0 rep 1.
    assert_eq!(tasks(&p.next(10, &calm())), vec![3]);
    p.ended(20, 3, Outcome::Done).unwrap();
    assert_eq!(tasks(&p.next(20, &calm())), vec![1]);
}

#[test]
fn big_task_not_starved_by_backfill() {
    // A 4-thread task waits behind a stream of 1-thread tasks on 4 cores.
    // Past the starvation time, nothing behind it starts until it fits.
    let mut w = vec![want(0, None, 0, 1, 1.0), want(0, None, 1, 4, 1.0)];
    for r in 2..40 {
        w.push(want(0, None, r, 1, 1.0));
    }
    let mut p = Pool::new(w, budget(4, 100.0)).unwrap();
    p.starve_ms = 1_000;
    let mut now = 0;
    let mut running = tasks(&p.next(now, &calm()));
    assert!(!running.contains(&1));
    let mut big_at = None;
    while big_at.is_none() && now < 100_000 {
        now += 100;
        let done = running.remove(0);
        p.ended(now, done, Outcome::Done).unwrap();
        let l = tasks(&p.next(now, &calm()));
        if l.contains(&1) {
            big_at = Some(now);
        }
        running.extend(l);
    }
    let at = big_at.expect("the big task started");
    assert!(at <= 1_000 + 4 * 100 + 100, "started at {at} ms");
}

#[test]
fn prop_never_over_cores() {
    for seed in 1..60u64 {
        let mut g = Rng(seed * 0x9e37_79b9);
        let cores = 1 + g.below(16) as u32;
        let n = 5 + g.below(40) as usize;
        let mut w = Vec::new();
        for i in 0..n {
            let mut c = want(
                g.below(3) as usize,
                None,
                i as u32,
                1 + g.below(cores as u64) as u32,
                1.0,
            );
            if c.stage > 0 && i > 0 {
                c.upstream = vec![g.below(i as u64) as usize];
            }
            w.push(c);
        }
        let mut p = Pool::new(w, budget(cores, 1e6)).unwrap();
        let mut running: Vec<usize> = Vec::new();
        let mut now = 0;
        for _ in 0..400 {
            now += 1 + g.below(50);
            running.extend(tasks(&p.next(now, &calm())));
            assert!(p.booked() <= cores, "seed {seed}");
            if running.is_empty() {
                break;
            }
            let k = running.swap_remove(g.below(running.len() as u64) as usize);
            let o = if g.below(5) == 0 {
                Outcome::Failed
            } else {
                Outcome::Done
            };
            p.ended(now, k, o).unwrap();
        }
        assert!(p.finished(), "seed {seed}");
    }
}

#[test]
fn prop_gpu_never_double_booked() {
    for seed in 1..60u64 {
        let mut g = Rng(seed * 0x2545_f491);
        let slots = 1 + g.below(2) as u32;
        let ids = vec![0, 1, 2];
        let mut w = Vec::new();
        for i in 0..30 {
            let mut c = want(0, None, i, 1, 1.0);
            c.gpus = g.below(3) as u32;
            w.push(c);
        }
        let b = Budget {
            gpus: ids.clone(),
            gpu_slots: slots,
            ..budget(64, 1e6)
        };
        let mut p = Pool::new(w, b).unwrap();
        let mut running: Vec<Launch> = Vec::new();
        let mut now = 0;
        loop {
            now += 1;
            for l in p.next(now, &calm()) {
                let distinct = l.gpus.windows(2).all(|d| d[0] < d[1]);
                assert!(distinct, "distinct devices, seed {seed}");
                running.push(l);
            }
            for id in &ids {
                let used = running.iter().filter(|l| l.gpus.contains(id)).count();
                assert!(used as u32 <= slots, "seed {seed} gpu {id}");
            }
            if running.is_empty() {
                break;
            }
            let k = running.swap_remove(g.below(running.len() as u64) as usize);
            let o = if g.below(3) == 0 {
                Outcome::Timeout
            } else {
                Outcome::Done
            };
            p.ended(now, k.task, o).unwrap();
        }
        assert!(p.finished());
    }
}

#[test]
fn gpu_task_gets_its_count_and_releases_it_on_timeout() {
    let mut a = want(0, None, 0, 1, 1.0);
    a.gpus = 2;
    let mut b = want(0, None, 1, 1, 1.0);
    b.gpus = 1;
    let budget = gpu_budget(vec![4, 7], 1);
    let mut p = Pool::new(vec![a, b], budget).unwrap();
    let l = p.next(0, &calm());
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].gpus, vec![4, 7]);
    p.ended(5, 0, Outcome::Timeout).unwrap();
    let l = p.next(5, &calm());
    assert_eq!((l[0].task, l[0].gpus.len()), (1, 1));
}

#[test]
fn gpus_over_the_machine_refused() {
    let mut a = want(0, None, 0, 1, 1.0);
    a.gpus = 3;
    let budget = gpu_budget(vec![0, 1], 4);
    let e = Pool::new(vec![a], budget).unwrap_err().to_string();
    assert!(e.contains("3 GPUs"), "{e}");
}

#[test]
fn mem_budget_and_floor_block() {
    let w = vec![want(0, None, 0, 1, 6.0), want(0, None, 1, 1, 6.0)];
    // The budget fits one 6 GB task.
    let mut p = Pool::new(w.clone(), budget(8, 10.0)).unwrap();
    assert_eq!(p.next(0, &calm()).len(), 1);

    // The floor: 6 GB booked, 1 GB of it resident, so 5 GB more may come.
    // A new 6 GB task needs 5 + 6 + 4 GB available.
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    let tight = mem(Some(14.0));
    assert_eq!(p.next(0, &tight).len(), 1);
    p.rss(0, 1.0);
    assert!(p.next(1, &tight).is_empty());
    let enough = mem(Some(15.0));
    assert_eq!(tasks(&p.next(2, &enough)), vec![1]);
}

#[test]
fn no_floor_without_a_reading() {
    // An executor that leaves memory to its scheduler gives no reading.
    let w = vec![want(0, None, 0, 1, 6.0)];
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    let none = mem(None);
    assert_eq!(p.next(0, &none).len(), 1);
}

#[test]
fn disk_low_blocks_new_starts() {
    let w = vec![want(0, None, 0, 1, 1.0)];
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    let low = Live {
        disk_low: true,
        ..calm()
    };
    assert!(p.next(0, &low).is_empty());
    assert_eq!(p.next(1, &calm()).len(), 1);
}

#[test]
fn max_running_caps_a_method() {
    let mut w = Vec::new();
    for r in 0..4 {
        let mut c = want(0, Some("M1"), r, 1, 1.0);
        c.max_running = Some(2);
        w.push(c);
        w.push(want(0, Some("M2"), r, 1, 1.0));
    }
    let mut p = Pool::new(w.clone(), budget(16, 100.0)).unwrap();
    let l = tasks(&p.next(0, &calm()));
    let m1 = l
        .iter()
        .filter(|&&i| w[i].method.as_deref() == Some("M1"))
        .count();
    assert_eq!((m1, l.len()), (2, 6));
}

#[test]
fn downstream_of_a_failed_task_is_skipped() {
    let mut w = vec![
        want(0, None, 0, 1, 1.0),
        want(1, None, 0, 1, 1.0),
        want(2, None, 0, 1, 1.0),
    ];
    w[1].upstream = vec![0];
    w[2].upstream = vec![1];
    let mut p = Pool::new(w, budget(4, 100.0)).unwrap();
    p.next(0, &calm());
    p.ended(1, 0, Outcome::Failed).unwrap();
    assert_eq!(p.state(1), &State::Skipped { upstream: Some(0) });
    assert_eq!(p.state(2), &State::Skipped { upstream: Some(0) });
    assert!(p.finished());
}

#[test]
fn method_halt_spares_other_method() {
    // Stage 0 has methods M1 and M2; the first 3 M1 attempts fail.
    let mut w = Vec::new();
    for r in 0..6 {
        w.push(want(0, Some("M1"), r, 1, 1.0));
        w.push(want(0, Some("M2"), r, 1, 1.0));
    }
    let n = w.len();
    let mut d = want(1, None, 5, 1, 1.0);
    d.upstream = vec![10]; // downstream of M1 rep 5
    w.push(d);
    let mut p = Pool::new(w, budget(1, 100.0)).unwrap();
    let mut now = 0;
    let mut m1_failed = 0;
    while !p.finished() {
        now += 1;
        let l = p.next(now, &calm());
        let Some(c) = l.first() else { break };
        let o = if c.task % 2 == 0 {
            m1_failed += 1;
            Outcome::Failed
        } else {
            Outcome::Done
        };
        p.ended(now, c.task, o).unwrap();
    }
    assert_eq!(m1_failed, 3);
    assert_eq!(
        p.halted_methods(),
        vec![("S0".to_string(), Some("M1".to_string()))]
    );
    for i in (1..n).step_by(2) {
        assert_eq!(p.state(i), &State::Ended(Outcome::Done), "M2 task {i}");
    }
    assert_eq!(p.state(6), &State::Skipped { upstream: None });
    assert_eq!(p.state(n), &State::Skipped { upstream: Some(10) });
    assert_eq!(p.halted(), None);
}

#[test]
fn half_failed_after_ten_halts_a_method() {
    let mut w = Vec::new();
    for r in 0..20 {
        w.push(want(0, Some("M1"), r, 1, 1.0));
    }
    let mut p = Pool::new(w, budget(1, 100.0)).unwrap();
    // done, done, fail, fail, ... : the first 3 are not all failures.
    for i in 0..10 {
        let l = p.next(i, &calm());
        let o = if i < 2 || i % 2 == 1 {
            Outcome::Done
        } else {
            Outcome::Failed
        };
        p.ended(i, l[0].task, o).unwrap();
    }
    // 10 ended, 4 failed: still running.
    assert!(p.halted_methods().is_empty());
    for i in 10..12 {
        let l = p.next(i, &calm());
        p.ended(i, l[0].task, Outcome::Failed).unwrap();
    }
    // 12 ended, 6 failed: half.
    assert_eq!(p.halted_methods().len(), 1);
}

#[test]
fn benchmark_halts_when_every_method_of_a_stage_halts() {
    let mut w = Vec::new();
    for r in 0..3 {
        w.push(want(0, Some("M1"), r, 1, 1.0));
        w.push(want(0, Some("M2"), r, 1, 1.0));
    }
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    for l in p.next(0, &calm()) {
        p.ended(1, l.task, Outcome::Lost).unwrap();
    }
    let h = p.halted().expect("halted");
    assert!(h.contains("S0"), "{h}");
}

#[test]
fn benchmark_halts_when_a_stage_without_methods_halts() {
    let w: Vec<_> = (0..3).map(|r| want(0, None, r, 1, 1.0)).collect();
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    for l in p.next(0, &calm()) {
        p.ended(1, l.task, Outcome::Oom).unwrap();
    }
    assert!(p.halted().is_some());
}

#[test]
fn person_stop_not_counted() {
    let w: Vec<_> = (0..4).map(|r| want(0, Some("M1"), r, 1, 1.0)).collect();
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    let l = p.next(0, &calm());
    p.ended(1, l[0].task, Outcome::Stopped).unwrap();
    p.ended(1, l[1].task, Outcome::Stopped).unwrap();
    p.ended(1, l[2].task, Outcome::Failed).unwrap();
    p.ended(1, l[3].task, Outcome::Stopped).unwrap();
    // One counted attempt, and it failed: no halt yet.
    assert!(p.halted_methods().is_empty());
}

#[test]
fn a_person_stops_a_pending_task() {
    let mut w = vec![want(0, None, 0, 1, 1.0), want(1, None, 0, 1, 1.0)];
    w[1].upstream = vec![0];
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    p.stop_pending(0, 0).unwrap();
    assert_eq!(p.state(0), &State::Ended(Outcome::Stopped));
    assert_eq!(p.state(1), &State::Skipped { upstream: Some(0) });
    assert!(p.next(0, &calm()).is_empty());
}

#[test]
fn drain_launches_nothing() {
    let w: Vec<_> = (0..4).map(|r| want(0, None, r, 1, 1.0)).collect();
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    let l = p.next(0, &calm());
    assert_eq!(l.len(), 2);
    p.drain(1);
    p.ended(2, l[0].task, Outcome::Done).unwrap();
    assert!(p.next(2, &calm()).is_empty());
    assert!(!p.finished());
    p.ended(3, l[1].task, Outcome::Done).unwrap();
    assert!(p.finished());
}

#[test]
fn lower_cores_no_preemption() {
    let w: Vec<_> = (0..6).map(|r| want(0, None, r, 2, 1.0)).collect();
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    let l = p.next(0, &calm());
    assert_eq!(l.len(), 4);
    p.set_cores(1, 4);
    // Nothing is stopped: 8 stay booked over a limit of 4.
    assert_eq!(p.booked(), 8);
    p.ended(2, l[0].task, Outcome::Done).unwrap();
    assert!(p.next(2, &calm()).is_empty());
    p.ended(3, l[1].task, Outcome::Done).unwrap();
    p.ended(3, l[2].task, Outcome::Done).unwrap();
    // 2 booked of 4: one more 2-thread task.
    assert_eq!(p.next(3, &calm()).len(), 1);
}

#[test]
fn resume_adopts_and_reuses() {
    let mut w = vec![
        want(0, None, 0, 2, 1.0),
        want(0, None, 1, 2, 1.0),
        want(1, None, 0, 1, 1.0),
    ];
    w[2].upstream = vec![0];
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    p.resume(0, &[0], &[1]).unwrap();
    assert_eq!(p.booked(), 2);
    // Task 2 is ready (its upstream was reused) but the adopted task
    // holds every core.
    assert!(p.next(0, &calm()).is_empty());
    p.ended(5, 1, Outcome::Done).unwrap();
    assert_eq!(tasks(&p.next(5, &calm())), vec![2]);
    assert!(p.resume(5, &[], &[1]).is_err(), "only a pending task");
}

#[test]
fn ended_on_a_task_not_running_is_an_error() {
    let mut p = Pool::new(vec![want(0, None, 0, 1, 1.0)], budget(2, 100.0)).unwrap();
    assert!(p.ended(0, 0, Outcome::Done).is_err());
}

#[test]
fn a_task_over_the_budget_is_refused() {
    let e = Pool::new(vec![want(0, None, 0, 9, 1.0)], budget(8, 100.0))
        .unwrap_err()
        .to_string();
    assert!(e.contains("9 threads"), "{e}");
    let e = Pool::new(vec![want(0, None, 0, 1, 200.0)], budget(8, 100.0))
        .unwrap_err()
        .to_string();
    assert!(e.contains("200 GB"), "{e}");
}

#[test]
fn accounting_exact_scripted() {
    // 4 cores, in core-ms:
    //   0..10   A(2) and B(1) run; C(2) is ready and does not fit: no_fit 1,
    //           booked 3. D(1) waits for A.
    //   10..15  A ended at 10; C and D wait for the next round: slack 3,
    //           booked 1.
    //   15..25  B(1), C(2) run; D(1, method M1 capped at 1 with C) is held
    //           by the cap: capped 1, booked 3.
    //   25..30  B and C ended at 25, D launched at 25: booked 1; nothing
    //           else ready: idle 3.
    //   30..40  drained, D runs: booked 1, idle 3.
    let mut a = want(0, None, 0, 2, 1.0);
    a.id = "A".into();
    let mut b = want(0, None, 1, 1, 1.0);
    b.id = "B".into();
    let mut c = want(0, Some("M1"), 2, 2, 1.0);
    c.max_running = Some(1);
    let mut d = want(0, Some("M1"), 3, 1, 1.0);
    d.max_running = Some(1);
    d.upstream = vec![0];
    let mut p = Pool::new(vec![a, b, c, d], budget(4, 100.0)).unwrap();
    assert_eq!(tasks(&p.next(0, &calm())), vec![0, 1]);
    p.ended(10, 0, Outcome::Done).unwrap();
    assert_eq!(tasks(&p.next(15, &calm())), vec![2]);
    p.ended(25, 2, Outcome::Done).unwrap();
    p.ended(25, 1, Outcome::Done).unwrap();
    assert_eq!(tasks(&p.next(25, &calm())), vec![3]);
    p.drain(30);
    let acct = p.account(40);
    assert_eq!(
        acct,
        Acct {
            booked: 3 * 10 + 5 + 3 * 10 + 5 + 10,
            slack: 3 * 5,
            no_fit: 10,
            capped: 10,
            blocked: 0,
            idle: 3 * 5 + 3 * 10,
        }
    );
    assert_eq!(
        acct.booked + acct.slack + acct.no_fit + acct.capped + acct.blocked + acct.idle,
        4 * 40
    );
}

#[test]
fn free_cores_take_the_best_hold() {
    // 4 cores: A (method M1, capped at 1) and B run, 2 booked. C needs 3
    // cores (no_fit) and D waits for the M1 cap (capped): the free cores
    // are no_fit, which comes first.
    let mut a = want(0, Some("M1"), 0, 1, 1.0);
    a.max_running = Some(1);
    let b = want(0, None, 1, 1, 1.0);
    let c = want(0, None, 2, 3, 1.0);
    let mut d = want(0, Some("M1"), 3, 1, 1.0);
    d.max_running = Some(1);
    let mut p = Pool::new(vec![a, b, c, d], budget(4, 100.0)).unwrap();
    assert_eq!(tasks(&p.next(0, &calm())), vec![0, 1]);
    let acct = p.account(10);
    assert_eq!((acct.no_fit, acct.capped), (20, 0));
}

#[test]
fn a_capped_task_reserves_nothing() {
    // 2 cores. Method M1 runs one at a time; M2's tasks sit behind M1's in
    // order and wait for cores.
    let mut w = Vec::new();
    for r in 0..3 {
        let mut c = want(0, Some("M1"), r, 1, 1.0);
        c.max_running = Some(1);
        w.push(c);
    }
    for r in 3..6 {
        w.push(want(0, Some("M2"), r, 1, 1.0));
    }
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    p.starve_ms = 10;
    assert_eq!(tasks(&p.next(0, &calm())), vec![0, 3]);
    // Tasks 1 and 2 have waited past starve_ms, for the cap: freeing cores
    // cannot help them, so M2's next task takes the free core.
    p.ended(100, 3, Outcome::Done).unwrap();
    assert_eq!(tasks(&p.next(100, &calm())), vec![4]);
}

#[test]
fn nothing_is_reserved_when_nothing_of_ours_runs() {
    // A task over the memory floor on an idle benchmark holds nothing back.
    let w = vec![want(0, None, 0, 1, 50.0), want(0, None, 1, 1, 1.0)];
    let mut p = Pool::new(w, budget(8, 100.0)).unwrap();
    p.starve_ms = 0;
    let live = mem(Some(20.0));
    assert_eq!(tasks(&p.next(0, &live)), vec![1]);
}

#[test]
fn cores_reserved_for_a_starved_task_are_no_fit() {
    // 2 cores: A(1) runs, B(2) starves, C(1) fits but is held for B.
    let w = vec![
        want(0, None, 0, 1, 1.0),
        want(0, None, 1, 2, 1.0),
        want(0, None, 2, 1, 1.0),
    ];
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    p.starve_ms = 0;
    assert_eq!(tasks(&p.next(0, &calm())), vec![0]);
    let acct = p.account(10);
    assert_eq!((acct.slack, acct.no_fit), (0, 10));
}

#[test]
fn a_cycle_is_refused() {
    let mut w = vec![want(0, None, 0, 1, 1.0), want(1, None, 0, 1, 1.0)];
    w[0].upstream = vec![1];
    w[1].upstream = vec![0];
    let e = Pool::new(w, budget(8, 100.0)).unwrap_err().to_string();
    assert!(e.contains("not before it"), "{e}");
}

#[test]
fn a_method_has_one_cap() {
    let mut a = want(0, Some("M1"), 0, 1, 1.0);
    a.max_running = Some(2);
    let b = want(0, Some("M1"), 1, 1, 1.0);
    let e = Pool::new(vec![a, b], budget(8, 100.0))
        .unwrap_err()
        .to_string();
    assert!(e.contains("max_running"), "{e}");
}

/// The clock the tests and the fake executor share, in ms.
type Clock = std::rc::Rc<std::cell::Cell<u64>>;

/// Each launched task runs for `ms[task]`, then ends with `outcome[task]`.
struct Fake {
    clock: Clock,
    ms: Vec<u64>,
    outcome: Vec<Outcome>,
    running: Vec<(u64, usize)>,
    launched: Vec<usize>,
    /// The next launch fails.
    refuse: bool,
    /// Extra ends the next poll reports.
    extra: Vec<(usize, Outcome)>,
    /// How long a poll waits, in ms.
    poll_ms: u64,
}

/// A fake of `n` tasks, each running 10 ms and ending done.
fn fake(n: usize) -> Fake {
    Fake {
        clock: Clock::default(),
        ms: vec![10; n],
        outcome: vec![Outcome::Done; n],
        running: Vec::new(),
        launched: Vec::new(),
        refuse: false,
        extra: Vec::new(),
        poll_ms: 0,
    }
}

/// One [`round`] on `ex`'s clock.
fn tick(p: &mut Pool, ex: &mut Fake) -> anyhow::Result<Vec<Launch>> {
    let c = ex.clock.clone();
    let r = round(p, ex, &mut || c.get());
    match r.error {
        Some(e) => Err(e),
        None => Ok(r.launched),
    }
}

impl Executor for Fake {
    fn live(&mut self) -> anyhow::Result<Live> {
        Ok(calm())
    }
    fn launch(&mut self, tasks: &[Launch]) -> anyhow::Result<()> {
        if std::mem::take(&mut self.refuse) {
            anyhow::bail!("refused");
        }
        for l in tasks {
            self.running
                .push((self.clock.get() + self.ms[l.task], l.task));
            self.launched.push(l.task);
        }
        Ok(())
    }
    fn poll(&mut self) -> anyhow::Result<Vec<Ended>> {
        self.clock.set(self.clock.get() + self.poll_ms);
        let now = self.clock.get();
        let (ended, running) = self.running.iter().partition(|(at, _)| *at <= now);
        self.running = running;
        let mut out: Vec<Ended> = std::mem::take(&mut self.extra)
            .into_iter()
            .map(|(c, o)| Ended::bare(c, o))
            .collect();
        out.extend(
            ended
                .into_iter()
                .map(|(_, c): (u64, usize)| Ended::bare(c, self.outcome[c])),
        );
        Ok(out)
    }
    fn cancel(&mut self, _: &[usize]) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
fn round_drives_an_executor_to_the_end() {
    // Three tasks on 2 cores, then one reading the first; the second fails.
    let mut w: Vec<_> = (0..3).map(|r| want(0, None, r, 1, 1.0)).collect();
    let mut d = want(1, None, 0, 1, 1.0);
    d.upstream = vec![0];
    w.push(d);
    let mut ex = fake(4);
    ex.ms[1] = 5;
    ex.outcome[1] = Outcome::Failed;
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    while !p.finished() {
        tick(&mut p, &mut ex).unwrap();
        ex.clock.set(ex.clock.get() + 1);
        assert!(ex.clock.get() < 1000);
    }
    assert_eq!(ex.launched, vec![0, 1, 2, 3]);
    assert_eq!(p.state(1), &State::Ended(Outcome::Failed));
    assert_eq!(p.state(3), &State::Ended(Outcome::Done));
}

#[test]
fn a_refused_launch_leaves_the_tasks_pending() {
    let mut ex = fake(2);
    ex.refuse = true;
    let w: Vec<_> = (0..2).map(|r| want(0, None, r, 1, 1.0)).collect();
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    assert!(tick(&mut p, &mut ex).is_err());
    assert_eq!((p.state(0), p.booked()), (&State::Pending, 0));
    assert_eq!(tasks(&tick(&mut p, &mut ex).unwrap()), vec![0, 1]);
}

#[test]
fn a_bad_end_does_not_lose_the_others() {
    let mut ex = fake(2);
    let w: Vec<_> = (0..2).map(|r| want(0, None, r, 1, 1.0)).collect();
    let mut p = Pool::new(w, budget(2, 100.0)).unwrap();
    tick(&mut p, &mut ex).unwrap();
    ex.clock.set(10);
    // Task 0 is reported twice: the repeat is an error, and task 1, after
    // it, still ends.
    ex.extra = vec![(0, Outcome::Done)];
    assert!(tick(&mut p, &mut ex).is_err());
    assert_eq!(p.state(1), &State::Ended(Outcome::Done));
}

#[test]
fn ends_are_stamped_when_poll_returns() {
    // The task ends at 10; the poll that reports it waits 30 ms first.
    let mut ex = fake(1);
    let mut p = Pool::new(vec![want(0, None, 0, 2, 1.0)], budget(2, 100.0)).unwrap();
    tick(&mut p, &mut ex).unwrap();
    ex.clock.set(10);
    ex.poll_ms = 30;
    tick(&mut p, &mut ex).unwrap();
    assert_eq!(p.state(0), &State::Ended(Outcome::Done));
    assert_eq!(p.account(40).booked, 2 * 40);
}

/// A plan task of `stage` with id `id`, reading `upstream`.
fn plan_task(id: &str, stage: &str, method: Option<&str>, upstream: &[&str]) -> Task {
    Task {
        id: id.into(),
        point: Default::default(),
        rep: 0,
        rep_seed: 0,
        seed: 0,
        stage: stage.into(),
        method: method.map(str::to_string),
        argv: Vec::new(),
        env: Default::default(),
        threads: 2,
        mem_gb: 3.0,
        gpus: 1,
        timeout_secs: 60,
        max_running: Some(4),
        dir: Default::default(),
        makes: Vec::new(),
        upstream: upstream.iter().map(|u| u.to_string()).collect(),
        key: String::new(),
    }
}

#[test]
fn wants_of_a_plan() {
    let plan = vec![
        plan_task("P1.r0.sim", "sim", None, &[]),
        plan_task("P1.r0.fit-M1", "fit", Some("M1"), &["P1.r0.sim"]),
    ];
    let w = Want::of_plan(&plan, &["sim", "fit", "score"]).unwrap();
    assert_eq!((w[1].stage, w[1].method.as_deref()), (1, Some("M1")));
    assert_eq!(w[1].upstream, vec![0]);
    assert_eq!(
        (w[1].threads, w[1].mem_gb, w[1].gpus, w[1].max_running),
        (2, 3.0, 1, Some(4))
    );

    let e = Want::of_plan(&plan, &["sim"]).unwrap_err().to_string();
    assert!(e.contains("stage fit"), "{e}");
    let bad = vec![plan_task("C1", "sim", None, &["C9"])];
    let e = Want::of_plan(&bad, &["sim"]).unwrap_err().to_string();
    assert!(e.contains("C9"), "{e}");
}

#[test]
fn two_plan_tasks_with_one_id_are_refused() {
    let plan = vec![
        plan_task("C1", "sim", None, &[]),
        plan_task("C1", "sim", None, &[]),
    ];
    let e = Want::of_plan(&plan, &["sim"]).unwrap_err().to_string();
    assert!(e.contains("two tasks"), "{e}");
}

#[test]
fn resume_reuses_and_adopts_in_one_change() {
    let mut d = want(1, None, 0, 1, 1.0);
    d.upstream = vec![0];
    let w = vec![want(0, None, 0, 1, 1.0), want(0, None, 1, 1, 1.0), d];
    let mut p = Pool::new(w, budget(4, 100.0)).unwrap();
    p.resume(0, &[0], &[1]).unwrap();
    assert_eq!(p.state(0), &State::Ended(Outcome::Done));
    assert_eq!(p.state(1), &State::Running(Vec::new()));
    assert_eq!(p.running().collect::<Vec<_>>(), vec![1]);
    // Its upstream done, task 2 is ready.
    assert_eq!(tasks(&p.next(1, &calm())), vec![2]);
    assert!(p.resume(2, &[0], &[]).is_err());
}

/// sim (0) -> fit M1 (1, capped at 1) and fit M2 (2) -> score M1 (3) and
/// score M2 (4), for one rep.
fn chain() -> Vec<Want> {
    let mut sim = want(0, None, 0, 1, 1.0);
    sim.id = "sim".into();
    let mut fit1 = want(1, Some("M1"), 0, 1, 1.0);
    fit1.upstream = vec![0];
    fit1.max_running = Some(1);
    let mut fit2 = want(1, Some("M2"), 0, 1, 1.0);
    fit2.upstream = vec![0];
    let mut score1 = want(2, Some("M1"), 0, 1, 1.0);
    score1.upstream = vec![1];
    let mut score2 = want(2, Some("M2"), 0, 1, 1.0);
    score2.upstream = vec![2];
    vec![sim, fit1, fit2, score1, score2]
}

#[test]
fn ahead_every_task_goes_in_at_once_and_caps_are_the_schedulers() {
    let mut w = chain();
    let mut extra = want(1, Some("M1"), 1, 1, 1.0);
    extra.upstream = vec![0];
    extra.max_running = Some(1);
    w.push(extra);
    let mut p = Pool::new(w, budget(100, 100.0)).unwrap();
    p.queue_ahead();
    let mut got = tasks(&p.next(0, &calm()));
    got.sort_unstable();
    // Both of M1's fits too: its cap is the array's throttle.
    assert_eq!(got, [0, 1, 2, 3, 4, 5]);
    // Without it, nothing behind the sim goes in.
    let mut p = Pool::new(chain(), budget(100, 100.0)).unwrap();
    assert_eq!(tasks(&p.next(0, &calm())), [0]);
}

#[test]
fn ahead_what_waits_on_a_failed_task_is_cancelled_and_skipped() {
    let mut p = Pool::new(chain(), budget(100, 100.0)).unwrap();
    p.queue_ahead();
    assert_eq!(p.next(0, &calm()).len(), 5);
    p.ended(1, 0, Outcome::Done).unwrap();
    p.ended(2, 1, Outcome::Failed).unwrap();
    // fit M1 failed: score M1, queued behind it, is to be cancelled.
    assert_eq!(p.cancels(), [3]);
    p.cancelled();
    assert!(p.cancels().is_empty());
    // Cancelled before it ran: skipped for fit M1, not stopped.
    p.ended(3, 3, Outcome::Stopped).unwrap();
    assert_eq!(*p.state(3), State::Skipped { upstream: Some(1) });
    // A task refused by its wrapper, its upstream not done, before the
    // pool hears of the upstream's end, is skipped too.
    p.ended(4, 4, Outcome::Stopped).unwrap();
    assert_eq!(*p.state(4), State::Skipped { upstream: Some(2) });
    p.ended(5, 2, Outcome::Done).unwrap();
    assert!(p.finished());
}

#[test]
fn ahead_a_halted_methods_queued_tasks_are_cancelled() {
    // Three fits of M1, one each rep, all failing: M1 halts.
    let mut w = vec![want(0, None, 0, 1, 1.0)];
    for rep in 0..4 {
        let mut f = want(1, Some("M1"), rep, 1, 1.0);
        f.upstream = vec![0];
        w.push(f);
    }
    let mut p = Pool::new(w, budget(100, 100.0)).unwrap();
    p.queue_ahead();
    assert_eq!(p.next(0, &calm()).len(), 5);
    p.ended(1, 0, Outcome::Done).unwrap();
    for i in 1..=3 {
        p.ended(2, i, Outcome::Failed).unwrap();
    }
    assert_eq!(p.halted_methods().len(), 1);
    assert_eq!(p.cancels(), [4]);
    p.ended(3, 4, Outcome::Stopped).unwrap();
    assert_eq!(*p.state(4), State::Skipped { upstream: None });
}
