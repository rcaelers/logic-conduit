use std::sync::OnceLock;
use std::thread;

use crossbeam_channel::{Sender, bounded};

type Job = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct WorkerPool {
    sender: Sender<Job>,
    workers: usize,
}

impl WorkerPool {
    fn new() -> Self {
        let workers = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .clamp(1, 8);
        // Decoders bound their own in-flight work. This shared queue only
        // absorbs simultaneous submissions from several decoder nodes.
        let (sender, receiver) = bounded::<Job>(workers * 4);
        for index in 0..workers {
            let receiver = receiver.clone();
            thread::Builder::new()
                .name(format!("dsl-compute-{index}"))
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    }
                })
                .expect("failed to start shared DSL compute worker");
        }
        Self { sender, workers }
    }

    pub(crate) fn workers(&self) -> usize {
        self.workers
    }

    pub(crate) fn spawn(&self, job: impl FnOnce() + Send + 'static) -> Result<(), ()> {
        self.sender.send(Box::new(job)).map_err(|_| ())
    }
}

pub(crate) fn shared_worker_pool() -> &'static WorkerPool {
    static POOL: OnceLock<WorkerPool> = OnceLock::new();
    POOL.get_or_init(WorkerPool::new)
}
