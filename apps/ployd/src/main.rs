fn main() {
    let components = [
        ploy_platform::crate_marker(),
        ploy_trading::crate_marker(),
        ploy_deployments::crate_marker(),
        ploy_connectivity::crate_marker(),
        ploy_operator_contracts::crate_marker(),
    ];

    eprintln!("ployd workspace skeleton: {}", components.join(", "));
}
