use std::{
    collections::BinaryHeap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use clap::Parser;
use futures::{StreamExt, stream::FuturesUnordered};
use tokio::sync::Mutex;
use volo_http::{client::Client, error::client::Result};

fn init() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
}

// 32 MiB
const STACK_SIZE: usize = 32 << 20;
const DEFAULT_THREAD_NUM: usize = 4;

#[derive(Debug, Parser)]
#[command(
    name = "bench-client",
    author,
    about = "HTTP Bench client",
    rename_all = "kebab-case",
    arg_required_else_help = true
)]
struct Args {
    /// Target address of server (IP:PORT)
    addr: SocketAddr,
    /// Number of worker threads
    #[arg(short = 'n', long = "num-worker", default_value_t = DEFAULT_THREAD_NUM)]
    num_worker: usize,
}

async fn bench(
    client: volo_http::client::DefaultClient,
    req_count: Arc<AtomicUsize>,
    err_count: Arc<AtomicUsize>,
    lat_buf: Arc<DualBuffer<LatencyReporter>>,
) {
    loop {
        let start_time = Instant::now();
        let res = client.get("/").send().await;
        let lat = start_time.elapsed();
        req_count.fetch_add(1, Ordering::Relaxed);
        let Ok(resp) = res else {
            err_count.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if !resp.status().is_success() {
            err_count.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        lat_buf.first().lock().await.push(lat.as_micros() as usize);
    }
}

struct LatencyReporter {
    sum: usize,
    heap: BinaryHeap<usize>,
}

impl LatencyReporter {
    const fn new() -> Self {
        Self {
            sum: 0,
            heap: BinaryHeap::new(),
        }
    }

    fn push(&mut self, val: usize) {
        self.sum += val;
        self.heap.push(val);
    }

    fn report(&mut self) -> (usize, usize) {
        let sum = self.sum;
        self.sum = 0;
        let len = self.heap.len();
        if len == 0 {
            return (0, 0);
        }
        let avg = sum / len;
        let percent = len / 100;
        for _ in 0..percent {
            if self.heap.pop().is_none() {
                return (0, 0);
            }
        }
        let Some(p99) = self.heap.pop() else {
            return (0, 0);
        };
        self.heap.clear();
        (avg, p99)
    }
}

struct DualBuffer<T> {
    first: Arc<Mutex<T>>,
    second: Arc<Mutex<T>>,
    swap: AtomicUsize,
}

impl<T> DualBuffer<T> {
    fn new(first: T, second: T) -> Self {
        Self {
            first: Arc::new(Mutex::new(first)),
            second: Arc::new(Mutex::new(second)),
            swap: AtomicUsize::new(0),
        }
    }

    fn first(&self) -> &Arc<Mutex<T>> {
        if self.swap.load(Ordering::Acquire) & 1 == 0 {
            &self.first
        } else {
            &self.second
        }
    }

    fn swap_and_second(&self) -> &Arc<Mutex<T>> {
        if self.swap.fetch_add(1, Ordering::AcqRel) & 1 == 1 {
            &self.second
        } else {
            &self.first
        }
    }
}

async fn qps_reporter(
    req_count: Arc<AtomicUsize>,
    err_count: Arc<AtomicUsize>,
    lat_buf: Arc<DualBuffer<LatencyReporter>>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let qps = req_count.swap(0, Ordering::Relaxed);
        let eps = err_count.swap(0, Ordering::Relaxed);
        let err_rate = (eps as f64) / (qps as f64) * 100.0;
        let (avg, p99) = lat_buf.swap_and_second().lock().await.report();
        let (avg, p99) = (avg as f64 / 1000.0, p99 as f64 / 1000.0);
        println!("QPS: {qps}, ERR: {eps}, ERR%: {err_rate:.2}, avg: {avg:.2} ms, p99: {p99:.2} ms");
    }
}

async fn real_main(addr: SocketAddr, num: usize) -> Result<()> {
    let mut builder = Client::builder();
    builder.address(addr);
    let client = builder.build()?;
    let req_count = Arc::new(AtomicUsize::new(0));
    let err_count = Arc::new(AtomicUsize::new(0));
    let lat_buf = Arc::new(DualBuffer::new(
        LatencyReporter::new(),
        LatencyReporter::new(),
    ));

    let mut futs = FuturesUnordered::new();
    for _ in 0..num {
        futs.push(tokio::spawn(bench(
            client.clone(),
            req_count.clone(),
            err_count.clone(),
            lat_buf.clone(),
        )));
    }
    tokio::spawn(qps_reporter(req_count, err_count, lat_buf));

    while futs.next().await.is_some() {}

    Ok(())
}

fn main() {
    init();
    let args = Args::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(STACK_SIZE)
        .worker_threads(args.num_worker + 1)
        .build()
        .expect("Failed to build multi-thread runtime");

    let ret = rt.block_on(real_main(args.addr, args.num_worker));
    ret.expect("Failed to bench");
}
