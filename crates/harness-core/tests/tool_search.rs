use harness_core::tools::fs::ScopedFs;
use harness_core::tools::search::{GlobTool, GrepTool};
use harness_core::tools::{Tool, ToolInvocation, ToolRegistry};
use std::sync::Arc;

fn call(name: &str, arguments: serde_json::Value) -> ToolInvocation {
    ToolInvocation { call_id: name.into(), name: name.into(), arguments }
}

#[tokio::test]
async fn search_tools_dispatch_glob_and_grep_through_registry() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src/nested")).unwrap();
    std::fs::write(root.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.path().join("src/nested/lib.rs"), "pub fn answer() {}\n").unwrap();
    std::fs::write(root.path().join("README.md"), "answer docs\n").unwrap();

    let fs = Arc::new(ScopedFs::new(root.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(GlobTool::new(fs.clone())));
    registry.register(Arc::new(GrepTool::new(fs)));

    let glob = registry.execute(call("glob", serde_json::json!({
        "pattern": "**/*.rs"
    }))).await;
    assert!(glob.ok, "{}", glob.output);
    assert!(glob.output.contains("src/main.rs"));
    assert!(glob.output.contains("src/nested/lib.rs"));
    assert!(!glob.output.contains(".git"));

    let grep = registry.execute(call("grep", serde_json::json!({
        "pattern": "answer",
        "path": "src",
        "include": "*.rs"
    }))).await;
    assert!(grep.ok, "{}", grep.output);
    assert!(grep.output.contains("src/nested/lib.rs:1: pub fn answer() {}"));
    assert!(!grep.output.contains("README.md"));
}

#[test]
fn search_tools_expose_upstream_catalog_names() {
    let root = tempfile::tempdir().unwrap();
    let fs = Arc::new(ScopedFs::new(root.path()).unwrap());
    assert_eq!(GlobTool::new(fs.clone()).spec().name, "glob");
    assert_eq!(GrepTool::new(fs).spec().name, "grep");
}
