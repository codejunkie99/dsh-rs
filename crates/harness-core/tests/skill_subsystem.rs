use harness_core::approval::ApprovalPolicy;
use harness_core::harness::{HarnessSetupsConfig, SUPPORTED_TOOLS};
use harness_core::skills::{
    render_skill_content, FileSystemSkillProvider, SkillFileSystemConfig, SkillInvocationPolicy,
    SkillLookupOptions, SkillProvider, SkillRegistry, SkillResourceBase,
};
use harness_core::tools::{SkillTool, Tool, ToolInvocation};
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn bundled_skill(root: &Path, name: &str, description: &str, body: &str) {
    write(
        &root.join(name).join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    );
}

#[tokio::test]
async fn filesystem_registry_discovers_and_prioritizes_dsh_skill_roots() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let custom = tempfile::tempdir().unwrap();
    let bundled = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join(".git")).unwrap();
    fs::create_dir(project.path().join("src")).unwrap();

    bundled_skill(
        bundled.path(),
        "bundled-only",
        "Bundled only",
        "Bundled body.",
    );
    bundled_skill(bundled.path(), "same", "Bundled loses", "Bundled body.");
    bundled_skill(custom.path(), "custom-only", "Custom only", "Custom body.");
    bundled_skill(
        custom.path(),
        "runtime-name",
        "Custom loses",
        "Custom body.",
    );
    bundled_skill(
        &project.path().join(".dsh/skills"),
        "same",
        "Project wins",
        "Project body.",
    );
    bundled_skill(
        &project.path().join(".agents/skills"),
        "same",
        "Project agents loses",
        "Body.",
    );
    bundled_skill(
        &home.path().join(".dsh/skills"),
        "same",
        "User DSH loses",
        "User body.",
    );
    bundled_skill(
        &home.path().join(".agents/skills"),
        "same",
        "User agents loses",
        "User body.",
    );
    bundled_skill(
        &home.path().join(".dsh/skills"),
        "runtime-name",
        "User loses",
        "User body.",
    );
    write(
        &home.path().join(".dsh/skills/user-only/SKILL.md"),
        "---\nname: user-only\ndescription: User-only\ndisable-model-invocation: true\n---\n\nUser-only body.\n",
    );

    let mut registry = SkillRegistry::new();
    let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
        include_default_roots: true,
        dsh_home: home.path().join(".dsh"),
        agents_home: home.path().join(".agents"),
        custom_skill_dirs: vec![custom.path().to_path_buf()],
        bundled_skill_dir: Some(bundled.path().to_path_buf()),
    })
    .unwrap();
    registry.register_provider(Arc::new(provider)).unwrap();
    registry
        .register_runtime(
            "project-name",
            "Runtime loses to project",
            "Project body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "runtime-name",
            "Runtime wins",
            "Runtime body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let options = SkillLookupOptions {
        cwd: Some(project.path().join("src")),
    };
    let summaries = registry.list(&options).await.unwrap();
    let names: Vec<_> = summaries.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "bundled-only",
            "custom-only",
            "project-name",
            "runtime-name",
            "same",
            "user-only"
        ]
    );

    let same = summaries.iter().find(|skill| skill.name == "same").unwrap();
    assert_eq!(same.description, "Project wins");
    assert_eq!(same.source, "project-dsh");
    assert_eq!(same.provider, "filesystem");
    assert_eq!(
        same.resource_base,
        Some(SkillResourceBase::Directory {
            path: project.path().join(".dsh/skills/same")
        })
    );

    let project_name = registry
        .get("project-name", &options)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project_name.description, "Runtime loses to project");
    let runtime_name = registry
        .get("runtime-name", &options)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime_name.description, "Runtime wins");
    let user_only = registry.get("user-only", &options).await.unwrap().unwrap();
    assert!(!user_only.invocation.model_invocable);
}

#[tokio::test]
async fn filesystem_provider_parses_flat_skills_and_ignores_invalid_entries() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".dsh/skills");
    fs::create_dir_all(&root).unwrap();
    write(
        &root.join("flat-skill.md"),
        "---\nname: flat-skill\ndescription: Flat skill\n---\n\nFlat body.\n",
    );
    write(
        &root.join("rich-skill.md"),
        "---\nname: rich-skill\ndescription: Rich skill\nwhen_to_use: Use for rich parsing\ndisable-model-invocation: false\nuser-invocable: true\n---\n\nUse the rich instructions.\n",
    );
    write(
        &root.join("bad-name.md"),
        "---\nname: Bad_Name\ndescription: Bad\n---\n\nBad.\n",
    );
    write(
        &root.join("missing-description.md"),
        "---\nname: missing-description\n---\n\nBad.\n",
    );
    write(&root.join("no-frontmatter.md"), "# Notes\n");
    write(&root.join("ignored.txt"), "not a skill\n");

    let mut registry = SkillRegistry::new();
    let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
        include_default_roots: true,
        dsh_home: home.path().join(".dsh"),
        agents_home: home.path().join(".agents"),
        custom_skill_dirs: Vec::new(),
        bundled_skill_dir: None,
    })
    .unwrap();
    registry.register_provider(Arc::new(provider)).unwrap();

    let options = SkillLookupOptions { cwd: None };
    let summaries = registry.list(&options).await.unwrap();
    let names: Vec<_> = summaries.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, ["flat-skill", "rich-skill"]);

    let rich = registry.get("rich-skill", &options).await.unwrap().unwrap();
    assert_eq!(rich.when_to_use.as_deref(), Some("Use for rich parsing"));
    assert_eq!(rich.content, "Use the rich instructions.");
    assert!(rich.invocation.model_invocable);
    assert!(rich.invocation.user_invocable);
    assert_eq!(rich.path, Some(root.join("rich-skill.md")));
}

#[test]
fn skill_content_rendering_matches_the_dsh_wire_contract() {
    let rendered = render_skill_content(
        "escaped-name",
        "filesystem",
        Some(SkillResourceBase::Directory {
            path: "/skills/a & b".into(),
        }),
        "Trusted instructions.",
    );
    assert_eq!(
        rendered,
        [
            "<skill_content name=\"escaped-name\">",
            "<skill_resources>",
            "Base directory for this skill: /skills/a &amp; b",
            "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.",
            "</skill_resources>",
            "",
            "<skill_instructions>",
            "Trusted instructions.",
            "</skill_instructions>",
            "</skill_content>"
        ]
        .join("\n")
    );
}

#[tokio::test]
async fn skill_tool_loads_model_skills_and_rejects_unavailable_skills() {
    let mut registry = SkillRegistry::new();
    registry
        .register_runtime(
            "model-skill",
            "A model skill",
            "Follow the model instructions.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "user-skill",
            "A user skill",
            "Follow the user instructions.",
            SkillInvocationPolicy {
                model_invocable: false,
                user_invocable: true,
            },
        )
        .unwrap();
    let tool = SkillTool::new(Arc::new(registry));

    let spec = tool.spec();
    assert_eq!(spec.name, "skill");
    assert_eq!(
        spec.parameters,
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The exact skill name from the available skills list."
                }
            },
            "required": ["name"]
        })
    );

    let loaded = tool
        .execute(ToolInvocation {
            call_id: "call-skill".into(),
            name: "skill".into(),
            arguments: serde_json::json!({"name": "model-skill"}),
        })
        .await
        .unwrap();
    assert!(loaded.ok);
    assert!(loaded
        .output
        .contains("<skill_content name=\"model-skill\">"));
    assert!(loaded.output.contains("Follow the model instructions."));

    let invalid = tool
        .execute(ToolInvocation {
            call_id: "call-invalid".into(),
            name: "skill".into(),
            arguments: serde_json::json!({"name": "Bad_Name"}),
        })
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("invalid skill name"));

    let unavailable = tool
        .execute(ToolInvocation {
            call_id: "call-unavailable".into(),
            name: "skill".into(),
            arguments: serde_json::json!({"name": "user-skill"}),
        })
        .await
        .unwrap_err();
    assert!(unavailable
        .to_string()
        .contains("not available for model invocation"));
}

#[tokio::test]
async fn runtime_registry_rejects_invalid_names_and_duplicate_providers() {
    let mut registry = SkillRegistry::new();
    let invalid = registry
        .register_runtime(
            "Bad_Name",
            "Invalid",
            "Body",
            SkillInvocationPolicy::default(),
        )
        .unwrap_err();
    assert!(invalid.to_string().contains("invalid skill name"));

    let provider: Arc<dyn SkillProvider> = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: Path::new("/tmp/dsh").into(),
            agents_home: Path::new("/tmp/agents").into(),
            custom_skill_dirs: Vec::new(),
            bundled_skill_dir: None,
        })
        .unwrap(),
    );
    registry.register_provider(provider.clone()).unwrap();
    let duplicate = registry.register_provider(provider).unwrap_err();
    assert!(duplicate.to_string().contains("already registered"));
}

#[test]
fn standard_harness_and_default_approval_admit_the_skill_tool() {
    assert!(SUPPORTED_TOOLS.contains(&"skill"));
    let setups = HarnessSetupsConfig::default();
    assert!(setups.get("standard").unwrap().enables("skill"));
    assert_eq!(
        ApprovalPolicy::default().decide("skill"),
        harness_core::approval::ApprovalDecision::Allow
    );
}
