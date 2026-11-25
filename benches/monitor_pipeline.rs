use std::{hint::black_box, sync::OnceLock, time::Duration};

use criterion::{criterion_group, criterion_main, Criterion};
use reqwest::Client;
use scraper::{Html, Selector};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinHandle,
};

const HTML: &str = r#"<html><head><meta property="og:title" content="Discord Invite"><meta name="twitter:creator" content="@discord"><title>Invite</title></head><body>fixture</body></html>"#;
const SELECTOR_TEXT: &str = r#"meta[name="twitter:creator"][content="@discord"]"#;
static SELECTOR: OnceLock<Selector> = OnceLock::new();

fn selector() -> &'static Selector {
    SELECTOR.get_or_init(|| Selector::parse(SELECTOR_TEXT).expect("valid static selector"))
}

// Kept byte-for-byte equivalent in behavior to the production head-boundary scan.
fn closing_head_end(bytes: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"</head>";
    bytes
        .windows(NEEDLE.len())
        .position(|window| {
            window
                .iter()
                .zip(NEEDLE)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        })
        .map(|start| start + NEEDLE.len())
}

async fn serve_connection(mut stream: TcpStream, response: &'static [u8]) {
    let mut chunk = [0u8; 2048];
    loop {
        let mut request = Vec::with_capacity(1024);
        loop {
            let read = match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if request.len() > 16 * 1024 {
                return;
            }
        }

        if stream.write_all(response).await.is_err() {
            return;
        }
    }
}

async fn start_local_http_server() -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback benchmark server");
    let address = listener.local_addr().expect("read local address");
    let body = b"<html><head><title>local fixture</title></head><body>ok</body></html>";
    let response = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        )
        .into_bytes()
        .into_boxed_slice(),
    );

    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(serve_connection(stream, response));
                }
                Err(_) => break,
            }
        }
    });

    (format!("http://{address}/invite/fixture"), task)
}

fn parsing_benchmarks(c: &mut Criterion) {
    c.bench_function("html/scan_closing_head", |b| {
        b.iter(|| black_box(closing_head_end(black_box(HTML.as_bytes()))))
    });

    let selector = selector();
    c.bench_function("html/parse_and_select_metadata", |b| {
        b.iter(|| {
            let document = Html::parse_document(black_box(HTML));
            black_box(document.select(selector).next().is_some())
        })
    });
}

fn scheduler_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create benchmark runtime");
    let semaphore = Semaphore::new(2);

    c.bench_function("scheduler/semaphore_acquire_release", |b| {
        b.to_async(&runtime).iter(|| async {
            let permit = semaphore.acquire().await.expect("semaphore open");
            black_box(&permit);
            drop(permit);
        })
    });
}

fn local_http_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("create HTTP benchmark runtime");

    let (url, server) = runtime.block_on(start_local_http_server());
    let client = Client::builder()
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(1)
        .tcp_nodelay(true)
        .build()
        .expect("build benchmark HTTP client");

    c.bench_function("http/reqwest_loopback_pooled_roundtrip", |b| {
        b.to_async(&runtime).iter(|| async {
            let response = client.get(&url).send().await.expect("local HTTP response");
            black_box(response.bytes().await.expect("read local response body"));
        })
    });

    runtime.block_on(async {
        server.abort();
    });
}

criterion_group!(
    benches,
    parsing_benchmarks,
    scheduler_benchmark,
    local_http_benchmark
);
criterion_main!(benches);
