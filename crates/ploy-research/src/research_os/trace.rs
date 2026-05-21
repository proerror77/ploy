use sha2::{Digest, Sha256};

pub fn trace_hash(
    hash_prev: Option<&str>,
    run_id: &str,
    event_type: &str,
    agent_name: &str,
    input_json: &serde_json::Value,
    output_json: &serde_json::Value,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hash_prev.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(event_type.as_bytes());
    hasher.update(b"\n");
    hasher.update(agent_name.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical_json(input_json).as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical_json(output_json).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn canonical_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).expect("serde_json::Value serialization is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_hash_changes_when_output_changes() {
        let input = serde_json::json!({"a": 1});
        let out_a = serde_json::json!({"candidate": "a"});
        let out_b = serde_json::json!({"candidate": "b"});
        let hash_a = trace_hash(
            None,
            "run-1",
            "generate",
            "research_manager",
            &input,
            &out_a,
        );
        let hash_b = trace_hash(
            None,
            "run-1",
            "generate",
            "research_manager",
            &input,
            &out_b,
        );
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn trace_hash_links_previous_hash() {
        let input = serde_json::json!({"a": 1});
        let output = serde_json::json!({"candidate": "a"});
        let root = trace_hash(
            None,
            "run-1",
            "generate",
            "research_manager",
            &input,
            &output,
        );
        let linked = trace_hash(
            Some(&root),
            "run-1",
            "generate",
            "research_manager",
            &input,
            &output,
        );
        assert_ne!(root, linked);
    }
}
