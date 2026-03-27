//! Integration tests for exec tool skills_dir PATH injection.

use openclaw_light::config::ExecConfig;
use openclaw_light::tools::exec::ExecTool;
use openclaw_light::tools::Tool;
use serde_json::json;

#[tokio::test]
async fn exec_finds_script_in_skills_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    // Create a simple script in skills_dir
    let script_path = skills_dir.join("greet");
    std::fs::write(&script_path, "#!/bin/sh\necho \"Hello from skill\"").unwrap();

    // Make it executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let cfg = ExecConfig {
        timeout_secs: 10,
        max_output_bytes: 8192,
        work_dir: tmp.path().to_string_lossy().into(),
        skills_dir: skills_dir.to_string_lossy().into(),
    };
    let tool = ExecTool::new(&cfg);

    let result = tool.execute(json!({"command": "greet"})).await.unwrap();
    assert!(result.contains("Exit code: 0"));
    assert!(result.contains("Hello from skill"));
}

#[test]
fn exec_skills_dir_in_description() {
    let cfg = ExecConfig {
        skills_dir: "/my/custom/skills".into(),
        ..ExecConfig::default()
    };
    let tool = ExecTool::new(&cfg);
    let desc = tool.description();
    assert!(
        desc.contains("/my/custom/skills"),
        "description should mention the skills dir"
    );
}
