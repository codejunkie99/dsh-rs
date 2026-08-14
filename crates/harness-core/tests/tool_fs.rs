use harness_core::tools::fs::{ReadFileTool, ScopedFs, WriteFileTool};
use harness_core::tools::{Tool, ToolInvocation, ToolRegistry};
use std::sync::Arc;

fn invocation(name: &str, arguments: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        call_id: format!("call_{name}"),
        name: name.into(),
        arguments,
    }
}

#[tokio::test]
async fn canonical_read_and_write_are_registered_and_dispatchable() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source.txt"), "alpha\nbeta\ngamma\n").unwrap();

    let fs = Arc::new(ScopedFs::new(root.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(fs.clone())));
    registry.register(Arc::new(WriteFileTool::new(fs)));

    let names: Vec<_> = registry.specs().into_iter().map(|schema| schema.name).collect();
    assert!(names.contains(&"read".to_string()));
    assert!(names.contains(&"write".to_string()));
    assert!(!names.contains(&"read_file".to_string()));
    assert!(!names.contains(&"write_file".to_string()));

    let read = registry
        .execute(invocation(
            "read",
            serde_json::json!({"file_path": "source.txt", "offset": 2, "limit": 1}),
        ))
        .await;
    assert!(read.ok, "{}", read.output);
    assert!(read.output.contains("2: beta"));
    assert!(read.output.contains("End of file"));

    let write = registry
        .execute(invocation(
            "write",
            serde_json::json!({"file_path": "created.txt", "content": "created"}),
        ))
        .await;
    assert!(write.ok, "{}", write.output);
    assert!(write.output.contains("<path>created.txt</path>"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("created.txt")).unwrap(),
        "created"
    );
}

#[test]
fn canonical_schemas_match_upstream_parameter_names() {
    let root = tempfile::tempdir().unwrap();
    let fs = Arc::new(ScopedFs::new(root.path()).unwrap());

    let read = ReadFileTool::new(fs.clone()).spec();
    assert_eq!(read.name, "read");
    assert_eq!(read.parameters["properties"]["file_path"]["type"], "string");
    assert_eq!(read.parameters["properties"]["offset"]["description"], "1-based first line to return. Defaults to 1.");
    assert_eq!(read.parameters["properties"]["limit"]["description"], "Maximum number of lines to return. Defaults to 2000.");

    let write = WriteFileTool::new(fs).spec();
    assert_eq!(write.name, "write");
    assert_eq!(write.parameters["properties"]["file_path"]["type"], "string");
    assert_eq!(write.parameters["properties"]["content"]["type"], "string");
}
