use harness_core::tools::fs::{EditFileTool, ScopedFs};
use harness_core::tools::{Tool, ToolInvocation, ToolRegistry};
use std::sync::Arc;

fn invocation(arguments: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        call_id: "call_edit".into(),
        name: "edit".into(),
        arguments,
    }
}

fn registry_with_edit(root: &std::path::Path) -> (ToolRegistry, Arc<ScopedFs>) {
    let filesystem = Arc::new(ScopedFs::new(root).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EditFileTool::new(filesystem.clone())));
    (registry, filesystem)
}

#[tokio::test]
async fn edit_tool_dispatches_through_registry_and_replaces_literal_text() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes/plan.md"), "alpha\nbeta\n").unwrap();

    let (registry, _filesystem) = registry_with_edit(root.path());

    // The real dispatch path: resolve the `edit` tool by name and execute it.
    let output = registry
        .execute(invocation(serde_json::json!({
            "file_path": "notes/plan.md",
            "old_string": "beta",
            "new_string": "gamma"
        })))
        .await;

    assert!(output.ok, "edit dispatch failed: {}", output.output);
    assert_eq!(
        output.output,
        "The file notes/plan.md has been updated successfully."
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes/plan.md")).unwrap(),
        "alpha\ngamma\n"
    );
}

#[tokio::test]
async fn edit_tool_reports_not_found_for_zero_matches() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "hello\n").unwrap();
    let (registry, _filesystem) = registry_with_edit(root.path());

    let output = registry
        .execute(invocation(serde_json::json!({
            "file_path": "a.txt",
            "old_string": "missing",
            "new_string": "replacement"
        })))
        .await;

    assert!(!output.ok);
    assert!(output
        .output
        .contains("old_string was not found in \"a.txt\""));
    // The file is untouched after a failed edit.
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "hello\n"
    );
}

#[tokio::test]
async fn edit_tool_requires_exact_single_match_without_replace_all() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "x x x\n").unwrap();
    let (registry, _filesystem) = registry_with_edit(root.path());

    let output = registry
        .execute(invocation(serde_json::json!({
            "file_path": "a.txt",
            "old_string": "x",
            "new_string": "y"
        })))
        .await;

    assert!(!output.ok);
    assert!(output
        .output
        .contains("old_string matched 3 times in \"a.txt\""));
    assert!(output.output.contains("set replace_all to true"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "x x x\n"
    );
}

#[tokio::test]
async fn edit_tool_replace_all_replaces_every_occurrence() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "x x x\n").unwrap();
    let (registry, _filesystem) = registry_with_edit(root.path());

    let output = registry
        .execute(invocation(serde_json::json!({
            "file_path": "a.txt",
            "old_string": "x",
            "new_string": "y",
            "replace_all": true
        })))
        .await;

    assert!(output.ok);
    assert_eq!(
        output.output,
        "The file a.txt has been updated. All occurrences were successfully replaced."
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "y y y\n"
    );
}

#[tokio::test]
async fn edit_tool_rejects_blank_empty_and_noop_arguments() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "x\n").unwrap();
    let (registry, _filesystem) = registry_with_edit(root.path());

    for arguments in [
        serde_json::json!({
            "file_path": "   ",
            "old_string": "x",
            "new_string": "y"
        }),
        serde_json::json!({
            "file_path": "a.txt",
            "old_string": "",
            "new_string": "y"
        }),
        serde_json::json!({
            "file_path": "a.txt",
            "old_string": "x",
            "new_string": "x"
        }),
        serde_json::json!({
            "file_path": "a.txt",
            "old_string": "x",
            "new_string": "y",
            "sandbox_permissions": "write"
        }),
    ] {
        let output = registry.execute(invocation(arguments)).await;
        assert!(!output.ok, "expected edit to fail");
    }
}

#[tokio::test]
async fn edit_tool_normalizes_crlf_and_restores_line_endings() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("crlf.txt"), "a\r\nb\r\n").unwrap();
    let (registry, _filesystem) = registry_with_edit(root.path());

    let output = registry
        .execute(invocation(serde_json::json!({
            "file_path": "crlf.txt",
            "old_string": "a",
            "new_string": "z"
        })))
        .await;

    assert!(output.ok);
    // The edited file keeps its CRLF style.
    assert_eq!(
        std::fs::read_to_string(root.path().join("crlf.txt")).unwrap(),
        "z\r\nb\r\n"
    );
}

#[test]
fn edit_tool_exposes_the_upstream_schema() {
    let root = tempfile::tempdir().unwrap();
    let filesystem = Arc::new(ScopedFs::new(root.path()).unwrap());
    let spec = EditFileTool::new(filesystem).spec();

    assert_eq!(spec.name, "edit");
    assert_eq!(
        spec.parameters,
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to edit, resolved by the filesystem backend."
                },
                "old_string": {
                    "type": "string",
                    "description": "Literal text to replace. Must match exactly."
                },
                "new_string": {
                    "type": "string",
                    "description": "Literal replacement text. Use an empty string to delete the match."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once."
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    );
}
