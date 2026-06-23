//! `rabs tasks` - list server tasks, plus the reusable [`TaskPoller`].
//!
//! `TaskPoller` is a shared primitive: future bulk operations (embed, encode-m4b)
//! import it to serialize work by waiting until the server's task queue drains.

use std::time::{Duration, Instant};

use clap::Subcommand;

use crate::api::{self, Client, Task};
use crate::error::Result;

#[derive(Subcommand)]
pub enum TasksCmd {
    /// List current server tasks.
    List {
        /// Block until all tasks finish (or the timeout elapses).
        #[arg(long)]
        wait: bool,
        /// Max seconds to wait when `--wait` is set.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Seconds between polls when `--wait` is set.
        #[arg(long, default_value_t = 2)]
        poll: u64,
    },
}

pub fn run(cmd: TasksCmd) -> Result<()> {
    let client = api::client_only()?;
    match cmd {
        TasksCmd::List {
            wait,
            timeout,
            poll,
        } => {
            print_tasks(&client.list_tasks()?);
            if wait {
                let poller = TaskPoller::new(
                    &client,
                    Duration::from_secs(timeout),
                    Duration::from_secs(poll),
                );
                match poller.wait_until_drained()? {
                    WaitResult::Drained => println!("all tasks drained"),
                    WaitResult::Timeout => println!("timed out waiting for tasks to drain"),
                }
            }
            Ok(())
        }
    }
}

fn print_tasks(tasks: &[Task]) {
    if tasks.is_empty() {
        println!("(no tasks)");
        return;
    }
    for t in tasks {
        let state = if t.is_finished { "done" } else { "running" };
        println!("[{state:>7}] {} {} - {}", t.id, t.action, t.title);
    }
}

/// Outcome of [`TaskPoller::wait_until_drained`].
#[derive(Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// All tasks finished before the deadline.
    Drained,
    /// The deadline elapsed with tasks still running.
    Timeout,
}

/// Polls `GET /api/tasks` until no unfinished task remains or a deadline passes.
pub struct TaskPoller<'a> {
    client: &'a Client,
    timeout: Duration,
    interval: Duration,
}

impl<'a> TaskPoller<'a> {
    pub fn new(client: &'a Client, timeout: Duration, interval: Duration) -> Self {
        // Floor a zero interval so `--poll 0` can't spin a tight loop that
        // hammers /api/tasks.
        let interval = if interval.is_zero() {
            Duration::from_secs(1)
        } else {
            interval
        };
        Self {
            client,
            timeout,
            interval,
        }
    }

    /// Block until the task queue drains or `timeout` elapses. Checks once
    /// before the first sleep so an already-empty queue returns immediately.
    pub fn wait_until_drained(&self) -> Result<WaitResult> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if tasks_drained(&self.client.list_tasks()?) {
                return Ok(WaitResult::Drained);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(WaitResult::Timeout);
            }
            // Never sleep past the deadline (avoids oversleeping in tight windows).
            std::thread::sleep(self.interval.min(deadline - now));
        }
    }
}

/// True when no task is still running. Pure (no I/O), so it is unit-testable.
fn tasks_drained(tasks: &[Task]) -> bool {
    tasks.iter().all(|t| t.is_finished)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(is_finished: bool) -> Task {
        Task {
            id: "t1".to_string(),
            action: "encode-m4b".to_string(),
            title: "Some Book".to_string(),
            description: String::new(),
            is_finished,
        }
    }

    #[test]
    fn drained_when_empty_or_all_finished() {
        assert!(tasks_drained(&[]));
        assert!(tasks_drained(&[task(true), task(true)]));
        assert!(!tasks_drained(&[task(true), task(false)]));
    }

    #[test]
    fn poller_floors_zero_interval() {
        let client = Client::new(&crate::api::AbsConfig {
            server: "http://127.0.0.1:0".to_string(),
            access_token: "t".to_string(),
            default_library: None,
        });
        let poller = TaskPoller::new(&client, Duration::from_secs(5), Duration::ZERO);
        assert_eq!(poller.interval, Duration::from_secs(1));
    }
}
