fn main() {
    let components = [
        ploy_platform::crate_marker(),
        ploy_operator_contracts::crate_marker(),
    ];

    eprintln!("ployctl workspace skeleton: {}", components.join(", "));
}
