mod benchmark;
mod due_process;
mod invariant;

use std::path::PathBuf;
use std::time::Duration;

use tokio::net::UnixStream;

use reloopy_ipc::messages::{
    Envelope, HealthReport, Hello, LeaseRenew, TestRequest, Welcome, msg_types,
};
use reloopy_ipc::wire;
use tracing::{error, info, warn};

use benchmark::{BenchmarkScorer, BenchmarksConfig};
use invariant::{InvariantRunner, InvariantsConfig};

const IDENTITY: &str = "judge";

#[derive(Debug, Clone)]
struct Config {
    sock_path: PathBuf,
    constitution_dir: PathBuf,
    heartbeat_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        let base_dir = home.join(".reloopy");

        let constitution_dir = std::env::var("RELOOPY_CONSTITUTION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("CARGO_MANIFEST_DIR")
                    .map(|d| PathBuf::from(d).join("../../../constitution"))
                    .unwrap_or_else(|_| PathBuf::from("constitution"))
            });

        Self {
            sock_path: base_dir.join("reloopy.sock"),
            constitution_dir,
            heartbeat_interval: Duration::from_secs(8),
        }
    }
}

fn new_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("{}-{}", IDENTITY, COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("reloopy-judge service starting");

    let config = Config::default();

    loop {
        match run_service(&config).await {
            Ok(()) => {
                info!("Service exited cleanly");
                break;
            }
            Err(e) => {
                error!("Service error: {}. Reconnecting in 5s...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn run_service(config: &Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let invariants_config = InvariantsConfig::load(&config.constitution_dir)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let benchmarks_config = BenchmarksConfig::load(&config.constitution_dir)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;

    let invariant_runner = InvariantRunner::new(invariants_config);
    let benchmark_scorer = BenchmarkScorer::new(benchmarks_config);

    info!(sock = %config.sock_path.display(), "Connecting to Boot");
    let stream = UnixStream::connect(&config.sock_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    let hello = Hello {
        protocol_version: "1.0".to_string(),
        capabilities: serde_json::json!(["test", "score"]),
    };

    let hello_envelope = Envelope {
        from: IDENTITY.to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::HELLO.to_string(),
        id: new_msg_id(),
        payload: serde_json::to_value(&hello)?,
        fds: Vec::new(),
    };

    wire::write_envelope(&mut writer, &hello_envelope).await?;
    info!("Hello sent, waiting for Welcome...");

    let welcome_envelope = wire::read_envelope(&mut reader).await?;

    if welcome_envelope.msg_type != msg_types::WELCOME {
        return Err(format!(
            "Expected Welcome, got: {} (payload: {})",
            welcome_envelope.msg_type, welcome_envelope.payload
        )
        .into());
    }

    let welcome: Welcome = serde_json::from_value(welcome_envelope.payload)?;
    info!(
        runlevel = welcome.runlevel,
        "Handshake complete — connected to Boot"
    );

    let mut heartbeat_interval = tokio::time::interval(config.heartbeat_interval);
    let mut tasks_processed: u64 = 0;

    loop {
        tokio::select! {
            _ = heartbeat_interval.tick() => {
                let health = HealthReport {
                    runlevel: welcome.runlevel,
                    memory_bytes: 0,
                    cpu_percent: 0.0,
                    tasks_processed,
                };

                let renew = LeaseRenew { health };
                let envelope = Envelope {
                    from: IDENTITY.to_string(),
                    to: "boot".to_string(),
                    msg_type: msg_types::LEASE_RENEW.to_string(),
                    id: new_msg_id(),
                    payload: serde_json::to_value(&renew)?,
                fds: Vec::new(),
                };

                wire::write_envelope(&mut writer, &envelope).await?;
                tracing::trace!("Heartbeat sent");
            }

            result = wire::read_envelope(&mut reader) => {
                let envelope = result?;

                match envelope.msg_type.as_str() {
                    msg_types::LEASE_ACK => {
                        tracing::trace!("LeaseAck received");
                    }
                    msg_types::SHUTDOWN => {
                        info!("Shutdown received: {}", envelope.payload);
                        return Ok(());
                    }
                    msg_types::RUNLEVEL_CHANGE => {
                        info!("Runlevel change: {}", envelope.payload);
                    }
                    msg_types::TEST_REQUEST => {
                        let test_result = handle_test_request(
                            &envelope,
                            &invariant_runner,
                            &benchmark_scorer,
                        ).await;

                        let response = Envelope {
                            from: IDENTITY.to_string(),
                            to: envelope.from.clone(),
                            msg_type: msg_types::TEST_RESULT.to_string(),
                            id: envelope.id.clone(),
                            payload: serde_json::to_value(&test_result)?,
                        fds: Vec::new(),
                        };
                        wire::write_envelope(&mut writer, &response).await?;
                        tasks_processed += 1;
                    }
                    other => {
                        warn!("Unhandled message type: {}", other);
                    }
                }
            }
        }
    }
}

async fn handle_test_request(
    envelope: &Envelope,
    invariant_runner: &InvariantRunner,
    benchmark_scorer: &BenchmarkScorer,
) -> reloopy_ipc::messages::TestResult {
    let request: TestRequest = match serde_json::from_value(envelope.payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            return reloopy_ipc::messages::TestResult {
                version: String::new(),
                verdict: reloopy_ipc::messages::TestVerdict::HardFail,
                invariant_results: vec![],
                dimension_scores: vec![],
                overall_score: 0.0,
                suggestion: Some(format!("Invalid TestRequest payload: {}", e)),
            };
        }
    };

    info!(
        version = %request.version,
        binary = %request.binary_path,
        "Running tests"
    );

    let invariant_results = invariant_runner.run_all(&request.binary_path).await;

    let passed_count = invariant_results.iter().filter(|r| r.passed).count();
    let total_count = invariant_results.len();
    let protocol_compliance = if total_count > 0 {
        passed_count as f64 / total_count as f64
    } else {
        1.0
    };

    let raw_scores = vec![
        ("protocol_compliance".to_string(), protocol_compliance),
        ("task_correctness".to_string(), protocol_compliance),
        ("response_latency".to_string(), 1.0),
        ("resource_efficiency".to_string(), 1.0),
    ];

    let scoring = benchmark_scorer.score(&raw_scores);

    // TODO: retrieve old_overall from Boot state to detect regression
    let old_overall = None;

    let test_result = due_process::build_test_result(
        &request.version,
        &invariant_results,
        &scoring,
        old_overall,
        benchmark_scorer.regression_tolerance(),
    );

    info!(
        version = %request.version,
        verdict = ?test_result.verdict,
        overall_score = test_result.overall_score,
        passed = %format!("{}/{}", passed_count, total_count),
        "Test complete"
    );

    test_result
}
