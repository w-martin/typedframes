//! A small scoped-thread `map`, so per-file work (indexing, checking) can use every
//! core without a dependency on a thread-pool crate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// Parsing deeply nested source recurses further than a default worker stack allows.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORKERS: usize = 16;
// Below this many items, starting threads costs more than it saves.
const MIN_PARALLEL_ITEMS: usize = 8;

fn worker_count(items: usize) -> usize {
    let configured = std::env::var("TYPEDFRAMES_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let workers = configured.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_WORKERS))
    });
    workers.min(items)
}

/// `items.iter().map(f).collect()`, spread across threads, results in input order.
pub(crate) fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    map_with_workers(items, worker_count(items.len()), f)
}

fn map_with_workers<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    if workers <= 1 || items.len() < MIN_PARALLEL_ITEMS {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let spawned = std::thread::Builder::new()
                .stack_size(WORKER_STACK_BYTES)
                .spawn_scoped(scope, || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let value = f(item);
                    if let Ok(mut slots) = results.lock() {
                        slots[i] = Some(value);
                    }
                });
            if spawned.is_err() {
                break;
            }
        }
    });
    let mut slots = results.into_inner().unwrap_or_default();
    // Items no worker claimed (every spawn failed) are done here.
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(f(&items[i]));
        }
    }
    slots.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_return_results_in_input_order() {
        let items: Vec<usize> = (0..200).collect();

        let doubled = parallel_map(&items, |n| n * 2);

        assert_eq!(doubled, items.iter().map(|n| n * 2).collect::<Vec<_>>());
    }

    #[test]
    fn test_should_handle_a_short_or_empty_input() {
        assert_eq!(parallel_map(&[1, 2, 3], |n| n + 1), vec![2, 3, 4]);
        assert!(parallel_map::<u8, u8>(&[], |n| *n).is_empty());
    }

    #[test]
    fn test_should_run_every_item_exactly_once() {
        let count = AtomicUsize::new(0);
        let items = vec![(); 500];

        parallel_map(&items, |_| count.fetch_add(1, Ordering::Relaxed));

        assert_eq!(count.load(Ordering::Relaxed), 500);
    }

    #[test]
    fn test_should_use_more_than_one_thread() {
        let items = vec![(); 64];

        let threads = map_with_workers(&items, 4, |_| {
            std::thread::sleep(std::time::Duration::from_millis(2));
            std::thread::current().id()
        });

        let distinct: std::collections::HashSet<_> = threads.into_iter().collect();
        assert!(distinct.len() > 1);
    }
}
