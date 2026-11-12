//! The process's Prometheus text (stardelt-operator#607 item 6).
//!
//! Served on the internal listener only (`NOVA_INTERNAL_ADDR`, 8081 in the platform
//! manifests); the ingress never exposes it. Series are deliberately coarse: one
//! counter per method and status code, never per path (unbounded cardinality).

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use axum::extract::Request;
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

static START: OnceLock<SystemTime> = OnceLock::new();
static REQUESTS: Mutex<BTreeMap<(String, u16), u64>> = Mutex::new(BTreeMap::new());

/// Record the process start. Call once, first thing in `main`.
pub fn init() {
    START.get_or_init(SystemTime::now);
}

/// Count one served request. Called by [`count_requests`]; public for the test.
pub fn record(method: &str, status: u16) {
    let Ok(mut counts) = REQUESTS.lock() else {
        return;
    };
    *counts.entry((method.to_string(), status)).or_insert(0) += 1;
}

/// The layer the public listener wears: count every response by method and status.
pub async fn count_requests(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let response = next.run(request).await;
    record(&method, response.status().as_u16());
    response
}

fn seconds_since(epoch: SystemTime) -> f64 {
    SystemTime::now()
        .duration_since(epoch)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

/// The exposition text. Pure function of the statics, so a test can call it.
pub fn render() -> String {
    let start = START.get().copied().unwrap_or_else(SystemTime::now);
    let mut out = String::new();
    out.push_str("# HELP nova_build_info Build information.\n# TYPE nova_build_info gauge\n");
    out.push_str(&format!(
        "nova_build_info{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str("# HELP nova_process_start_time_seconds Start time of the process since the Unix epoch.\n# TYPE nova_process_start_time_seconds gauge\n");
    out.push_str(&format!(
        "nova_process_start_time_seconds {:.3}\n",
        start
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64()
    ));
    out.push_str("# HELP nova_uptime_seconds Seconds since this process started.\n# TYPE nova_uptime_seconds gauge\n");
    out.push_str(&format!(
        "nova_uptime_seconds {:.3}\n",
        seconds_since(start)
    ));
    out.push_str("# HELP nova_http_requests_total HTTP requests served, by method and status code.\n# TYPE nova_http_requests_total counter\n");
    if let Ok(counts) = REQUESTS.lock() {
        for ((method, status), count) in counts.iter() {
            out.push_str(&format!(
                "nova_http_requests_total{{method=\"{method}\",status=\"{status}\"}} {count}\n"
            ));
        }
    }
    out
}

/// `GET /metrics`: Prometheus text, no session, no state.
pub async fn endpoint() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        render(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_reports_build_info_and_the_request_counter() {
        init();
        record("GET", 200);
        record("GET", 200);
        record("POST", 500);
        let text = render();
        assert!(
            text.contains(&format!(
                "nova_build_info{{version=\"{}\"}} 1",
                env!("CARGO_PKG_VERSION")
            )),
            "{text}"
        );
        assert!(
            text.contains("# TYPE nova_http_requests_total counter"),
            "{text}"
        );
        assert!(
            text.contains("nova_http_requests_total{method=\"GET\",status=\"200\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("nova_http_requests_total{method=\"POST\",status=\"500\"} 1"),
            "{text}"
        );
    }
}
