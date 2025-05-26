use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use clap::Parser;
use motore::service::Service;
use volo::net::Address;
use volo_http::{
    body::Body,
    context::ServerContext,
    request::Request,
    response::Response,
    server::{
        Server,
        route::{Router, get_service},
    },
};

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
    name = "bench-server",
    author,
    about = "HTTP Bench server",
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

fn qps_reporter(req_count: Arc<AtomicUsize>) {
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let qps = req_count.swap(0, Ordering::Relaxed);
        println!("QPS: {qps}");
    }
}

struct RouteService {
    req_count: Arc<AtomicUsize>,
}

impl Service<ServerContext, Request> for RouteService {
    type Response = Response;
    type Error = Infallible;

    async fn call(&self, _: &mut ServerContext, _: Request) -> Result<Self::Response, Self::Error> {
        self.req_count.fetch_add(1, Ordering::Relaxed);
        Ok(Response::new(Body::empty()))
    }
}

async fn real_main(addr: SocketAddr) {
    let req_count = Arc::new(AtomicUsize::new(0));
    {
        let req_count = req_count.clone();
        std::thread::spawn(move || qps_reporter(req_count));
    }
    let svc = RouteService { req_count };
    let router = Router::new().route("/", get_service(svc));
    Server::new(router).run(Address::from(addr)).await.unwrap();
}

fn main() {
    init();
    let args = Args::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(STACK_SIZE)
        .worker_threads(args.num_worker)
        .build()
        .expect("Failed to build multi-thread runtime");

    rt.block_on(real_main(args.addr));
}
