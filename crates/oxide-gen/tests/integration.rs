//! Integration tests for `oxide-gen`.
//!
//! These tests run the full pipeline (parse → emit) for each spec kind into a
//! temporary directory and then:
//!
//! 1. Confirm that all six expected artifacts exist.
//! 2. Parse the generated `src/lib.rs` and `src/main.rs` with `syn` to verify
//!    that the emitted Rust is at least syntactically valid.
//! 3. Parse `module.json` and `mcp.json` back into JSON and check key fields.
//! 4. Spot-check the `SKILL.md` frontmatter.

use std::path::Path;

use oxide_gen::{generate_from_path, ApiKind};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn assert_syntactically_valid_rust(path: &Path) {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    if let Err(e) = syn::parse_file(&raw) {
        panic!("emitted Rust at {path:?} did not parse:\n{e}\n--- source ---\n{raw}");
    }
}

#[test]
fn generates_openapi_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let report = generate_from_path(
        &fixture("petstore.yaml"),
        Some(ApiKind::OpenApi),
        tmp.path(),
        None,
    )
    .expect("generation");

    for name in [
        "Cargo.toml",
        "src/lib.rs",
        "src/main.rs",
        "SKILL.md",
        "mcp.json",
        "module.json",
    ] {
        let p = tmp.path().join(name);
        assert!(p.exists(), "expected {name} to exist");
    }
    assert_eq!(report.files.len(), 6);

    let lib = tmp.path().join("src/lib.rs");
    let main = tmp.path().join("src/main.rs");
    assert_syntactically_valid_rust(&lib);
    assert_syntactically_valid_rust(&main);

    let lib_src = std::fs::read_to_string(&lib).unwrap();
    assert!(lib_src.contains("pub struct Pet"), "Pet struct missing");
    assert!(
        lib_src.contains("pub enum PetStatus"),
        "PetStatus enum missing"
    );
    assert!(
        lib_src.contains("pub async fn list_pets"),
        "list_pets method missing"
    );
    assert!(
        lib_src.contains("pub async fn get_pet"),
        "get_pet method missing"
    );
    // Path substitution uses snake-cased identifier.
    assert!(lib_src.contains("{pet_id}"));

    let main_src = std::fs::read_to_string(&main).unwrap();
    assert!(main_src.contains("Command::ListPets"));
    assert!(main_src.contains("Command::GetPet"));

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join("module.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["id"], "pet-store");
    assert_eq!(manifest["kind"], "native");
    assert_eq!(manifest["spec_kind"], "openapi");
    assert!(manifest["operations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "list_pets"));

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join("mcp.json")).unwrap())
            .unwrap();
    assert_eq!(mcp["type"], "stdio");
    assert!(mcp["tools"].as_array().unwrap().len() >= 3);

    let skill = std::fs::read_to_string(tmp.path().join("SKILL.md")).unwrap();
    assert!(skill.starts_with("---"));
    assert!(skill.contains("name: pet-store"));
    assert!(skill.contains("kind: openapi"));
}

#[test]
fn generates_graphql_crate() {
    let tmp = tempfile::tempdir().unwrap();
    generate_from_path(
        &fixture("schema.graphql"),
        Some(ApiKind::GraphQl),
        tmp.path(),
        Some("gql_demo"),
    )
    .expect("generation");

    let lib = tmp.path().join("src/lib.rs");
    let main = tmp.path().join("src/main.rs");
    assert_syntactically_valid_rust(&lib);
    assert_syntactically_valid_rust(&main);

    let lib_src = std::fs::read_to_string(&lib).unwrap();
    assert!(lib_src.contains("pub struct User"));
    assert!(lib_src.contains("pub struct Post"));
    assert!(lib_src.contains("pub enum Role"));
    assert!(lib_src.contains("pub async fn user"));
    assert!(lib_src.contains("pub async fn create_post"));
    // GraphQL body posts a JSON envelope.
    assert!(lib_src.contains("\"query\": query"));

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join("mcp.json")).unwrap())
            .unwrap();
    assert_eq!(mcp["name"], "gql-demo");

    for name in [
        "Cargo.toml",
        "src/lib.rs",
        "src/main.rs",
        "SKILL.md",
        "mcp.json",
        "module.json",
        "tests/smoke.rs",
    ] {
        let p = tmp.path().join(name);
        assert!(p.exists(), "expected {name} to exist");
    }

    // Compile and run the generated smoke test.
    let status = std::process::Command::new("cargo")
        .arg("test")
        .current_dir(tmp.path())
        .status()
        .expect("failed to execute cargo test");
    assert!(
        status.success(),
        "cargo test in generated GraphQL crate failed"
    );
}

#[test]
fn generates_grpc_crate() {
    let tmp = tempfile::tempdir().unwrap();
    generate_from_path(
        &fixture("echo.proto"),
        Some(ApiKind::Grpc),
        tmp.path(),
        None,
    )
    .expect("generation");

    for name in [
        "Cargo.toml",
        "src/lib.rs",
        "src/main.rs",
        "SKILL.md",
        "mcp.json",
        "module.json",
        "build.rs",
        "proto/schema.proto",
        "tests/smoke.rs",
    ] {
        let p = tmp.path().join(name);
        assert!(p.exists(), "expected {name} to exist");
    }

    let lib = tmp.path().join("src/lib.rs");
    let main = tmp.path().join("src/main.rs");
    assert_syntactically_valid_rust(&lib);
    assert_syntactically_valid_rust(&main);

    let lib_src = std::fs::read_to_string(&lib).unwrap();
    assert!(lib_src.contains("pub mod proto"));
    assert!(lib_src.contains("pub use proto::SayRequest;"));
    assert!(lib_src.contains("pub use proto::SayResponse;"));
    assert!(lib_src.contains("pub async fn say"));
    assert!(lib_src.contains("EchoClient::connect"));

    // Compile and run the generated smoke test.
    let status = std::process::Command::new("cargo")
        .arg("test")
        .current_dir(tmp.path())
        .status()
        .expect("failed to execute cargo test");
    assert!(status.success(), "cargo test in generated crate failed");
}

#[test]
fn kind_inference_picks_openapi_for_yaml() {
    let inferred = ApiKind::infer_from_path(Path::new("foo.yaml"));
    assert_eq!(inferred, Some(ApiKind::OpenApi));
    assert_eq!(
        ApiKind::infer_from_path(Path::new("foo.graphql")),
        Some(ApiKind::GraphQl)
    );
    assert_eq!(
        ApiKind::infer_from_path(Path::new("foo.proto")),
        Some(ApiKind::Grpc)
    );
}
