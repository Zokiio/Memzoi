use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn digest_sources(root: &Path, sources: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for source in sources {
        let path = root.join(source);
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        println!("cargo:rerun-if-changed={}", path.display());
        hasher.update(&(source.len() as u64).to_le_bytes());
        hasher.update(source.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    hasher.finalize().to_hex().to_string()
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing manifest directory"));
    let metrics = digest_sources(&root, &["src/recall_eval_v3.rs"]);
    let runner = digest_sources(
        &root,
        &[
            "src/recall_eval_v3.rs",
            "src/recall_candidate_eval.rs",
            "src/models.rs",
            "src/okf.rs",
            "src/search.rs",
            "src/service.rs",
        ],
    );
    println!("cargo:rustc-env=MEMZOI_RECALL_V3_METRICS_SOURCE_DIGEST={metrics}");
    println!("cargo:rustc-env=MEMZOI_RECALL_V3_RUNNER_SOURCE_DIGEST={runner}");
}
