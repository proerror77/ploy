#[tokio::main]
async fn main() {
    ploy_runner_host::run_with_args(std::env::args().collect()).await;
}
