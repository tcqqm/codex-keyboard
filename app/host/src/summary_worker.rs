#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ScopedJoinHandle};
use std::time::{Duration, Instant};

#[cfg(not(test))]
use crate::cache::CacheLimits;
use crate::cache::CacheStore;
use crate::paths::AppPaths;
#[cfg(not(test))]
use crate::secrets::{ImportLock, KeychainAccounts, LocalCacheSecretStore};
use crate::store::{StateStore, StoreError};
use crate::summary_orchestrator::{
    BundledPhraseSynthesizer, FixedPhraseGenerator, SummaryOrchestrator, SummaryRunOutcome,
    bundled_completion_pcm, completion_phrase,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const FAILURE_BACKOFF: Duration = Duration::from_secs(30);
const MANUAL_BACKOFF: Duration = Duration::from_secs(60 * 60);
// 四个槽可以同时合成「任务N已完成」。同一 task 一次只做一份。
const MAX_PARALLEL_SUMMARIES: usize = 4;

pub fn run(_paths: AppPaths, store: StateStore, cache: Arc<CacheStore>, shutdown: Arc<AtomicBool>) {
    let runtime = PhraseRuntime { cache };
    let mut retry_after = HashMap::<String, Instant>::new();
    run_ready(&store, &shutdown, &mut retry_after, &runtime);
}

struct PhraseRuntime {
    cache: Arc<CacheStore>,
}

#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn open_shared_cache(paths: &AppPaths) -> Result<CacheStore, String> {
    let lock = ImportLock::acquire(&paths.runtime_directory.join("key-import.lock"))
        .map_err(|error| error.to_string())?;
    let accounts = KeychainAccounts::load_or_create(&paths.installation_id, &lock)
        .map_err(|error| error.to_string())?;
    let cache_secrets = LocalCacheSecretStore::new(paths.cache_secret.clone(), &accounts);
    CacheStore::initialize(
        &paths.cache_directory,
        &cache_secrets,
        &accounts,
        CacheLimits::default(),
    )
    .map_err(|error| error.to_string())
}

enum TaskFinish {
    ClearRetry,
    RetryAfter(Duration),
}

trait SummaryTaskRunner: Sync {
    fn run_task(&self, store: StateStore, task_id: &str) -> TaskFinish;
}

impl SummaryTaskRunner for PhraseRuntime {
    fn run_task(&self, mut store: StateStore, task_id: &str) -> TaskFinish {
        let phrase = match phrase_for_task(&store, task_id) {
            Ok(phrase) => phrase,
            Err(error) => {
                eprintln!("summary_worker=failed error={error}");
                return TaskFinish::RetryAfter(FAILURE_BACKOFF);
            }
        };
        let Some(pcm) = bundled_completion_pcm(phrase) else {
            eprintln!("summary_worker=failed error=bundled phrase is missing");
            return TaskFinish::RetryAfter(FAILURE_BACKOFF);
        };
        let generator = FixedPhraseGenerator { phrase };
        let tts = BundledPhraseSynthesizer { pcm };
        let request_id = format!("auto-{}", uuid::Uuid::new_v4());
        let result = {
            let mut orchestrator =
                SummaryOrchestrator::new(&mut store, self.cache.as_ref(), &generator, &tts);
            match orchestrator.resume(task_id) {
                Ok(SummaryRunOutcome::Idle) => orchestrator.run(task_id, &request_id),
                other => other,
            }
        };
        match result {
            Ok(SummaryRunOutcome::Published { unread, .. }) => {
                eprintln!(
                    "summary_worker=published slot_phrase generation={} coverage={}",
                    unread.generation, unread.coverage_count
                );
                TaskFinish::ClearRetry
            }
            Ok(SummaryRunOutcome::AlreadyPublished { generation }) => {
                eprintln!("summary_worker=already_published generation={generation}");
                TaskFinish::ClearRetry
            }
            Ok(SummaryRunOutcome::Idle) => TaskFinish::ClearRetry,
            Ok(SummaryRunOutcome::ManualTtsReconciliationRequired { generation }) => {
                eprintln!("summary_worker=manual_reconciliation generation={generation}");
                TaskFinish::RetryAfter(MANUAL_BACKOFF)
            }
            Err(error) => {
                eprintln!("summary_worker=failed error={error}");
                TaskFinish::RetryAfter(FAILURE_BACKOFF)
            }
        }
    }
}

fn phrase_for_task(store: &StateStore, task_id: &str) -> Result<&'static str, StoreError> {
    let slot = store
        .bindings()?
        .into_iter()
        .filter(|binding| binding.task_id == task_id)
        .map(|binding| binding.slot)
        .min();
    Ok(completion_phrase(slot))
}

struct InFlight<'scope> {
    task_id: String,
    handle: ScopedJoinHandle<'scope, TaskFinish>,
}

fn run_ready<R: SummaryTaskRunner + Sync>(
    store: &StateStore,
    shutdown: &AtomicBool,
    retry_after: &mut HashMap<String, Instant>,
    runner: &R,
) {
    let mut scan_after = None::<String>;
    thread::scope(|scope| {
        let mut inflight: Vec<InFlight<'_>> = Vec::new();
        loop {
            reap_finished(&mut inflight, retry_after);
            if shutdown.load(Ordering::Acquire) {
                if inflight.is_empty() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            let tasks = match store.summary_work_tasks_after(scan_after.as_deref()) {
                Ok(tasks) => tasks,
                Err(error) => {
                    eprintln!("summary_worker=store_error error={error}");
                    sleep_until_shutdown(shutdown, FAILURE_BACKOFF);
                    continue;
                }
            };
            if tasks.is_empty() {
                scan_after = None;
                sleep_until_shutdown(shutdown, POLL_INTERVAL);
                continue;
            }
            let mut started = 0_usize;
            let mut stopped_early = false;
            for task_id in &tasks {
                if shutdown.load(Ordering::Acquire) {
                    stopped_early = true;
                    break;
                }
                if inflight.iter().any(|item| item.task_id == *task_id) {
                    continue;
                }
                if retry_after
                    .get(task_id)
                    .is_some_and(|deadline| *deadline > Instant::now())
                {
                    continue;
                }
                if inflight.len() >= MAX_PARALLEL_SUMMARIES {
                    stopped_early = true;
                    break;
                }
                let child = match store.open_worker_store() {
                    Ok(child) => child,
                    Err(error) => {
                        eprintln!("summary_worker=store_error error={error}");
                        retry_after.insert(task_id.clone(), Instant::now() + FAILURE_BACKOFF);
                        continue;
                    }
                };
                let owned_id = task_id.clone();
                let handle = scope.spawn(move || {
                    panic::catch_unwind(AssertUnwindSafe(|| runner.run_task(child, &owned_id)))
                        .unwrap_or_else(|_| {
                            eprintln!("summary_worker=failed error=panic");
                            TaskFinish::RetryAfter(FAILURE_BACKOFF)
                        })
                });
                inflight.push(InFlight {
                    task_id: task_id.clone(),
                    handle,
                });
                started += 1;
            }
            if !stopped_early {
                scan_after = tasks.last().cloned();
            }
            if started == 0 || inflight.len() >= MAX_PARALLEL_SUMMARIES {
                sleep_until_shutdown(shutdown, POLL_INTERVAL);
            }
        }
    });
}

fn reap_finished(inflight: &mut Vec<InFlight<'_>>, retry_after: &mut HashMap<String, Instant>) {
    let pending = std::mem::take(inflight);
    for item in pending {
        if !item.handle.is_finished() {
            inflight.push(item);
            continue;
        }
        match item.handle.join() {
            Ok(TaskFinish::ClearRetry) => {
                retry_after.remove(&item.task_id);
            }
            Ok(TaskFinish::RetryAfter(duration)) => {
                retry_after.insert(item.task_id, Instant::now() + duration);
            }
            Err(_) => {
                eprintln!("summary_worker=failed error=panic");
                retry_after.insert(item.task_id, Instant::now() + FAILURE_BACKOFF);
            }
        }
    }
}

fn sleep_until_shutdown(shutdown: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !shutdown.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(
            Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::{SummaryTaskRunner, TaskFinish, run_ready};
    use crate::cache::CacheId;
    use crate::store::{RolloutCursor, StateStore, SummaryClaimResult};

    const LAST: &str = "019fa972-5cfa-75e1-9008-0b17ade9a345";

    struct Stats {
        current: usize,
        peak: usize,
        started: Vec<String>,
    }

    struct OverlapRunner {
        release_batch: AtomicBool,
        release_last: AtomicBool,
        stats: Mutex<Stats>,
        cv: Condvar,
    }

    impl SummaryTaskRunner for OverlapRunner {
        fn run_task(&self, mut store: StateStore, task_id: &str) -> TaskFinish {
            let is_last = task_id == LAST;
            {
                let mut stats = self.stats.lock().expect("overlap stats");
                stats.current += 1;
                stats.peak = stats.peak.max(stats.current);
                stats.started.push(task_id.to_owned());
                self.cv.notify_all();
                let deadline = Instant::now() + Duration::from_secs(4);
                while Instant::now() < deadline {
                    let release = if is_last {
                        self.release_last.load(Ordering::Acquire)
                    } else {
                        self.release_batch.load(Ordering::Acquire)
                    };
                    if release {
                        break;
                    }
                    let (guard, _) = self
                        .cv
                        .wait_timeout(stats, Duration::from_millis(20))
                        .expect("overlap wait");
                    stats = guard;
                }
                stats.current -= 1;
            }
            let claimed = store
                .claim_summary(task_id, &format!("overlap-{task_id}"))
                .expect("claim")
                .expect("work remained");
            let SummaryClaimResult::Claimed(claim) = claimed else {
                panic!("expected a new claim");
            };
            let cache = CacheId::for_task(task_id, claim.generation)
                .expect("cache id")
                .reference();
            store.publish_summary(&claim, &cache).expect("publish");
            TaskFinish::ClearRetry
        }
    }

    fn insert_completion(
        store: &mut StateStore,
        task: &str,
        turn: &str,
        generation: u64,
        offset: u64,
        expected: Option<&RolloutCursor>,
    ) -> RolloutCursor {
        let cursor = RolloutCursor {
            task_id: task.to_owned(),
            rollout_path: PathBuf::from("/tmp/codex-keyboard-summary-overlap.jsonl"),
            device: 1,
            inode: 7,
            offset,
            generation,
            anchor: [1; 32],
        };
        store
            .commit_rollout_completion(expected, &cursor, turn, r#"{"v":1}"#)
            .expect("completion");
        cursor
    }

    #[test]
    fn four_ready_tasks_run_together_and_a_fifth_waits() {
        const FIRST: &str = "019fa972-5cfa-75e1-9008-0b17ade9a341";
        const SECOND: &str = "019fa972-5cfa-75e1-9008-0b17ade9a342";
        const THIRD: &str = "019fa972-5cfa-75e1-9008-0b17ade9a343";
        const FOURTH: &str = "019fa972-5cfa-75e1-9008-0b17ade9a344";
        let temp = tempdir().unwrap();
        let path = temp.path().join("state.sqlite3");
        let mut store = StateStore::open(&path).unwrap();
        let first = insert_completion(
            &mut store,
            FIRST,
            "019fa972-5cfa-75e1-9008-0b17ade9a351",
            1,
            100,
            None,
        );
        insert_completion(
            &mut store,
            FIRST,
            "019fa972-5cfa-75e1-9008-0b17ade9a356",
            2,
            200,
            Some(&first),
        );
        insert_completion(
            &mut store,
            SECOND,
            "019fa972-5cfa-75e1-9008-0b17ade9a352",
            1,
            100,
            None,
        );
        insert_completion(
            &mut store,
            THIRD,
            "019fa972-5cfa-75e1-9008-0b17ade9a353",
            1,
            100,
            None,
        );
        insert_completion(
            &mut store,
            FOURTH,
            "019fa972-5cfa-75e1-9008-0b17ade9a354",
            1,
            100,
            None,
        );
        insert_completion(
            &mut store,
            LAST,
            "019fa972-5cfa-75e1-9008-0b17ade9a355",
            1,
            100,
            None,
        );

        let runner = Arc::new(OverlapRunner {
            release_batch: AtomicBool::new(false),
            release_last: AtomicBool::new(false),
            stats: Mutex::new(Stats {
                current: 0,
                peak: 0,
                started: Vec::new(),
            }),
            cv: Condvar::new(),
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_runner = Arc::clone(&runner);
        let worker = thread::spawn(move || {
            let mut retry_after = HashMap::new();
            run_ready(
                &store,
                &worker_shutdown,
                &mut retry_after,
                worker_runner.as_ref(),
            );
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stats = runner.stats.lock().expect("overlap stats");
        while stats.peak < 4 && Instant::now() < deadline {
            let (guard, _) = runner
                .cv
                .wait_timeout(stats, Duration::from_millis(50))
                .expect("overlap wait");
            stats = guard;
        }
        assert_eq!(stats.peak, 4, "four summaries should overlap");
        assert!(!stats.started.iter().any(|task| task.as_str() == LAST));
        drop(stats);
        thread::sleep(Duration::from_millis(500));
        {
            let stats = runner.stats.lock().expect("overlap stats");
            assert_eq!(stats.peak, 4);
            assert_eq!(stats.started.len(), 4);
            assert_eq!(
                stats
                    .started
                    .iter()
                    .filter(|task| task.as_str() == FIRST)
                    .count(),
                1
            );
        }
        runner.release_batch.store(true, Ordering::Release);
        runner.cv.notify_all();

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stats = runner.stats.lock().expect("overlap stats");
        while !stats.started.iter().any(|task| task.as_str() == LAST) && Instant::now() < deadline {
            let (guard, _) = runner
                .cv
                .wait_timeout(stats, Duration::from_millis(50))
                .expect("overlap wait");
            stats = guard;
        }
        assert!(
            stats.started.iter().any(|task| task.as_str() == LAST),
            "the fifth task should start after a slot frees"
        );
        assert_eq!(stats.peak, 4);
        drop(stats);
        thread::sleep(Duration::from_millis(400));
        {
            let stats = runner.stats.lock().expect("overlap stats");
            assert_eq!(
                stats
                    .started
                    .iter()
                    .filter(|task| task.as_str() == LAST)
                    .count(),
                1
            );
            assert_eq!(stats.peak, 4);
        }
        shutdown.store(true, Ordering::Release);
        runner.release_last.store(true, Ordering::Release);
        runner.cv.notify_all();
        worker.join().expect("summary worker");

        let store = StateStore::open(&path).expect("reopen");
        assert!(store.summary_work_tasks_after(None).unwrap().is_empty());
        assert_eq!(
            store
                .current_unread_summary(FIRST)
                .unwrap()
                .unwrap()
                .coverage_count,
            2
        );
        for task in [SECOND, THIRD, FOURTH, LAST] {
            assert_eq!(
                store
                    .current_unread_summary(task)
                    .unwrap()
                    .unwrap()
                    .coverage_count,
                1
            );
        }
    }
}
