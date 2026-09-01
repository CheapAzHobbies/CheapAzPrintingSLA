//! Ordered parallel work over layers.
//!
//! Converting a print is a per-layer pipeline — decode the source image,
//! re-encode it for the destination — and every layer is independent. The
//! file itself is not: records have to be written in order, and a writer that
//! seeks around would lose the append-only, atomic-rename property the
//! conversion relies on.
//!
//! So the expensive half runs on a pool and the results are handed back in
//! index order, one at a time, on the calling thread. Nothing here knows what
//! a layer is; the caller supplies both halves.
//!
//! Two things are deliberately bounded. Workers are capped by how much memory
//! a layer costs, not just by core count: a 11520x5120 panel is 59MB per
//! layer, and thirty-two of those in flight is 1.9GB on a machine that may
//! not have it. And workers may only run a little ahead of the writer, so a
//! fast decoder cannot queue the whole print into memory.

use crate::error::{Error, FormatError, Result};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

/// The most memory in-flight decoded layers may take. Deliberately modest:
/// the pipeline is a speed-up, and one that makes a laptop swap is not.
const BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// How many layers may be worked on at once for a layer of `bytes_per_layer`.
///
/// Sized from the machine rather than fixed, so a 32-core desktop uses its
/// cores and a two-core laptop with a big print does not thrash.
pub fn workers_for(bytes_per_layer: u64) -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let budget = available_budget();
    if bytes_per_layer == 0 {
        return cpus;
    }
    let by_memory = (budget / bytes_per_layer).max(1) as usize;
    cpus.min(by_memory).max(1)
}

/// Never plan around more than a quarter of what the machine currently has
/// free, so a conversion started on a loaded machine backs off by itself.
fn available_budget() -> u64 {
    let quarter_free = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("MemAvailable:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024 / 4);
    match quarter_free {
        Some(bytes) => BUDGET_BYTES.min(bytes).max(64 * 1024 * 1024),
        None => BUDGET_BYTES,
    }
}

struct Shared<T> {
    /// Finished work waiting for its turn, keyed by index.
    done: BTreeMap<u32, T>,
    /// The next index to hand out to a worker.
    next_claim: u32,
    /// The next index the consumer wants. Workers stay near it.
    next_needed: u32,
    /// The first failure, from either side. Taken by whoever returns it, so
    /// it is not what the workers watch — see `stopped`.
    failed: Option<Error>,
}

/// Run `produce` for every index on a pool, and `consume` on the calling
/// thread in index order.
///
/// Stops at the first error from either side and returns it. `produce` runs on
/// worker threads and so must not touch anything that is not `Sync`.
pub fn in_order<T, P, C>(count: u32, workers: usize, produce: P, mut consume: C) -> Result<()>
where
    T: Send,
    P: Fn(u32) -> Result<T> + Sync,
    C: FnMut(u32, T) -> Result<()>,
{
    if count == 0 {
        return Ok(());
    }
    let workers = workers.max(1);
    // One layer per worker plus one in hand, which is enough to keep every
    // worker busy while the writer works through the backlog.
    let window = workers as u32 + 1;

    let state = Mutex::new(Shared::<T> {
        done: BTreeMap::new(),
        next_claim: 0,
        next_needed: 0,
        failed: None,
    });
    // Set once and never cleared. The error value itself is taken by whoever
    // returns it, so a worker cannot use it to tell that the run is over —
    // that mistake deadlocked the first version of this, with the workers
    // waiting for a window that the consumer was no longer advancing.
    let stopped = AtomicBool::new(false);
    let cv = Condvar::new();
    let produce = &produce;
    let (state, cv, stopped) = (&state, &cv, &stopped);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(move || loop {
                let index = {
                    let mut s = state.lock().unwrap();
                    loop {
                        if stopped.load(Ordering::Acquire) || s.next_claim >= count {
                            return;
                        }
                        if s.next_claim < s.next_needed + window {
                            let i = s.next_claim;
                            s.next_claim += 1;
                            break i;
                        }
                        s = cv.wait(s).unwrap();
                    }
                };
                let produced = produce(index);
                let mut s = state.lock().unwrap();
                match produced {
                    Ok(value) => {
                        s.done.insert(index, value);
                    }
                    Err(e) => {
                        if s.failed.is_none() {
                            s.failed = Some(e);
                        }
                        stopped.store(true, Ordering::Release);
                    }
                }
                drop(s);
                cv.notify_all();
            });
        }

        // The consumer runs here rather than on a thread of its own, so
        // `consume` can be `FnMut` and hold the output file.
        for index in 0..count {
            let value = {
                let mut s = state.lock().unwrap();
                loop {
                    if let Some(e) = s.failed.take() {
                        // Wake the workers so the scope can be left.
                        drop(s);
                        cv.notify_all();
                        return Err(e);
                    }
                    if stopped.load(Ordering::Acquire) {
                        drop(s);
                        cv.notify_all();
                        return Err(FormatError::Other("conversion stopped".into()).into());
                    }
                    if let Some(v) = s.done.remove(&index) {
                        s.next_needed = index + 1;
                        break v;
                    }
                    s = cv.wait(s).unwrap();
                }
            };
            cv.notify_all();
            if let Err(e) = consume(index, value) {
                stopped.store(true, Ordering::Release);
                cv.notify_all();
                return Err(e);
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn results_arrive_in_index_order() {
        let mut seen = Vec::new();
        in_order(
            200,
            8,
            |i| {
                // Deliberately uneven, so anything relying on the work
                // finishing in order would show up here.
                if i % 3 == 0 {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                Ok(i * 2)
            },
            |i, v| {
                assert_eq!(v, i * 2);
                seen.push(i);
                Ok(())
            },
        )
        .expect("pipeline");
        assert_eq!(seen, (0..200).collect::<Vec<_>>());
    }

    #[test]
    fn a_failure_stops_the_run_and_is_returned() {
        let err = in_order(
            500,
            8,
            |i| {
                if i == 40 {
                    Err(FormatError::Other("layer 40 is bad".into()).into())
                } else {
                    Ok(i)
                }
            },
            |_, _| Ok(()),
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("layer 40 is bad"), "{err}");
    }

    #[test]
    fn a_consumer_failure_stops_the_workers() {
        let produced = AtomicU32::new(0);
        let err = in_order(
            10_000,
            8,
            |i| {
                produced.fetch_add(1, Ordering::Relaxed);
                Ok(i)
            },
            |i, _| {
                if i == 5 {
                    Err(FormatError::Other("stop".into()).into())
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("stop"), "{err}");
        // The window is what keeps this small: without it the workers would
        // have run all ten thousand before the consumer reached index five.
        assert!(
            produced.load(Ordering::Relaxed) < 200,
            "{} layers produced after a failure at index 5",
            produced.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn every_index_is_produced_exactly_once() {
        let counts: Vec<AtomicU32> = (0..300).map(|_| AtomicU32::new(0)).collect();
        in_order(
            300,
            16,
            |i| {
                counts[i as usize].fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            |_, _| Ok(()),
        )
        .expect("pipeline");
        assert!(counts.iter().all(|c| c.load(Ordering::Relaxed) == 1));
    }

    #[test]
    fn one_worker_still_works() {
        let mut total = 0u32;
        in_order(50, 1, Ok, |_, v| {
            total += v;
            Ok(())
        })
        .expect("pipeline");
        assert_eq!(total, (0..50).sum::<u32>());
    }

    #[test]
    fn worker_count_is_bounded_by_layer_size() {
        // A 59MB layer must not put thirty-two of itself in flight.
        assert!(workers_for(11520 * 5120) <= 9);
        assert!(workers_for(11520 * 5120) >= 1);
        // A small layer is limited only by the machine.
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert_eq!(workers_for(1024), cpus);
    }
}
