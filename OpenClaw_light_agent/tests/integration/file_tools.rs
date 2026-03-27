//! Integration tests for file tools — complex multi-step operations.

use openclaw_light::tools::file::{FileEditTool, FileFindTool, FileReadTool, FileWriteTool};
use openclaw_light::tools::Tool;
use serde_json::json;

/// Write → Read → Edit → Read roundtrip: create a config file, edit a value,
/// verify the change.
#[tokio::test]
async fn write_edit_read_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let path_str = path.to_str().unwrap();

    let writer = FileWriteTool::new();
    let reader = FileReadTool::new();
    let editor = FileEditTool::new();

    // Step 1: Write a config file
    let content = "[server]\nhost = \"localhost\"\nport = 8080\n\n[database]\nurl = \"postgres://localhost/mydb\"\npool_size = 5\n";
    writer
        .execute(json!({"path": path_str, "content": content}))
        .await
        .unwrap();

    // Step 2: Read and verify
    let result = reader.execute(json!({"path": path_str})).await.unwrap();
    assert!(result.contains("port = 8080"));
    assert!(result.contains("pool_size = 5"));

    // Step 3: Edit port
    editor
        .execute(json!({
            "path": path_str,
            "old_string": "port = 8080",
            "new_string": "port = 3000"
        }))
        .await
        .unwrap();

    // Step 4: Edit database URL
    editor
        .execute(json!({
            "path": path_str,
            "old_string": "postgres://localhost/mydb",
            "new_string": "postgres://prod-server:5432/proddb"
        }))
        .await
        .unwrap();

    // Step 5: Verify both edits persisted
    let result = reader.execute(json!({"path": path_str})).await.unwrap();
    assert!(result.contains("port = 3000"));
    assert!(result.contains("prod-server:5432/proddb"));
    // Original values gone
    assert!(!result.contains("port = 8080"));
    assert!(!result.contains("localhost/mydb"));
}

/// Build a multi-file project tree, then search by name and content.
#[tokio::test]
async fn create_project_tree_and_find() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();

    let writer = FileWriteTool::new();
    let finder = FileFindTool::new();

    // Create a small project structure
    let files = vec![
        ("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n"),
        ("src/lib.rs", "pub mod utils;\npub mod config;\n"),
        (
            "src/utils.rs",
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n",
        ),
        (
            "src/config.rs",
            "pub struct Config {\n    pub port: u16,\n    pub host: String,\n}\n\nimpl Config {\n    pub fn default_port() -> u16 {\n        8080\n    }\n}\n",
        ),
        (
            "tests/test_utils.rs",
            "use myproject::utils::add;\n\n#[test]\nfn test_add() {\n    assert_eq!(add(2, 3), 5);\n}\n",
        ),
        ("Cargo.toml", "[package]\nname = \"myproject\"\nversion = \"0.1.0\"\n"),
        ("README.md", "# My Project\n\nA sample Rust project.\n"),
    ];

    for (rel_path, content) in &files {
        let full_path = dir.path().join(rel_path);
        writer
            .execute(json!({"path": full_path.to_str().unwrap(), "content": content}))
            .await
            .unwrap();
    }

    // Find all .rs files
    let result = finder
        .execute(json!({"path": root, "pattern": ".rs"}))
        .await
        .unwrap();
    assert!(result.contains("main.rs"));
    assert!(result.contains("lib.rs"));
    assert!(result.contains("utils.rs"));
    assert!(result.contains("config.rs"));
    assert!(result.contains("test_utils.rs"));
    // Non-.rs files excluded
    assert!(!result.contains("Cargo.toml"));
    assert!(!result.contains("README.md"));

    // Find files containing "pub fn"
    let result = finder
        .execute(json!({"path": root, "content": "pub fn"}))
        .await
        .unwrap();
    assert!(result.contains("utils.rs"));
    assert!(result.contains("config.rs"));
    // main.rs has "fn main" not "pub fn"
    assert!(!result.contains("main.rs"));

    // Combined: .rs files containing "Config"
    let result = finder
        .execute(json!({"path": root, "pattern": ".rs", "content": "Config"}))
        .await
        .unwrap();
    assert!(result.contains("config.rs"));
    assert!(!result.contains("utils.rs"));

    // Find Cargo.toml by name
    let result = finder
        .execute(json!({"path": root, "pattern": "Cargo"}))
        .await
        .unwrap();
    assert!(result.contains("Cargo.toml"));
}

/// Multi-line edit: refactor a function signature across multiple lines.
#[tokio::test]
async fn multiline_refactor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("handler.rs");
    let path_str = path.to_str().unwrap();

    let writer = FileWriteTool::new();
    let reader = FileReadTool::new();
    let editor = FileEditTool::new();

    let original = "pub async fn handle_request(\n    req: HttpRequest,\n    db: &Database,\n) -> Result<Response, Error> {\n    let user = db.get_user(req.user_id).await?;\n    Ok(Response::json(user))\n}\n";

    writer
        .execute(json!({"path": path_str, "content": original}))
        .await
        .unwrap();

    // Refactor: add a cache parameter and change return type
    editor
        .execute(json!({
            "path": path_str,
            "old_string": "pub async fn handle_request(\n    req: HttpRequest,\n    db: &Database,\n) -> Result<Response, Error> {",
            "new_string": "pub async fn handle_request(\n    req: HttpRequest,\n    db: &Database,\n    cache: &Cache,\n) -> Result<Response, AppError> {"
        }))
        .await
        .unwrap();

    let result = reader.execute(json!({"path": path_str})).await.unwrap();
    assert!(result.contains("cache: &Cache,"));
    assert!(result.contains("AppError"));
    assert!(!result.contains("Error {"));
    // Unchanged lines still present
    assert!(result.contains("let user = db.get_user"));
    assert!(result.contains("Response::json(user)"));
}

/// replace_all: rename a variable throughout a file.
#[tokio::test]
async fn rename_variable_with_replace_all() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.py");
    let path_str = path.to_str().unwrap();

    let writer = FileWriteTool::new();
    let reader = FileReadTool::new();
    let editor = FileEditTool::new();

    let content = "\
user_name = input(\"Name: \")\n\
print(f\"Hello, {user_name}\")\n\
if len(user_name) > 20:\n\
    print(f\"That's a long name, {user_name}!\")\n\
save_to_db(user_name)\n";

    writer
        .execute(json!({"path": path_str, "content": content}))
        .await
        .unwrap();

    // Rename user_name → display_name everywhere
    let result = editor
        .execute(json!({
            "path": path_str,
            "old_string": "user_name",
            "new_string": "display_name",
            "replace_all": true
        }))
        .await
        .unwrap();
    assert!(result.contains("5 occurrence"));

    let result = reader.execute(json!({"path": path_str})).await.unwrap();
    assert!(!result.contains("user_name"));
    // All 5 occurrences replaced
    let file_content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(file_content.matches("display_name").count(), 5);
}

/// Read with offset+limit: paginate through a large file.
#[tokio::test]
async fn paginated_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.csv");
    let path_str = path.to_str().unwrap();

    let writer = FileWriteTool::new();
    let reader = FileReadTool::new();

    // Generate a 100-line CSV
    let mut csv = String::from("id,name,value\n");
    for i in 1..=100 {
        csv.push_str(&format!("{i},item_{i},{}\n", i * 10));
    }
    writer
        .execute(json!({"path": path_str, "content": csv}))
        .await
        .unwrap();

    // Page 1: lines 1-10 (header + first 9 data rows)
    let page1 = reader
        .execute(json!({"path": path_str, "offset": 1, "limit": 10}))
        .await
        .unwrap();
    assert!(page1.contains("1\tid,name,value"));
    assert!(page1.contains("10\t9,item_9,90"));
    assert!(!page1.contains("11\t"));

    // Page 2: lines 11-20
    let page2 = reader
        .execute(json!({"path": path_str, "offset": 11, "limit": 10}))
        .await
        .unwrap();
    assert!(page2.contains("11\t10,item_10,100"));
    assert!(page2.contains("20\t19,item_19,190"));
    assert!(!page2.contains("21\t"));

    // Last page: lines 95-101
    let last = reader
        .execute(json!({"path": path_str, "offset": 95, "limit": 20}))
        .await
        .unwrap();
    assert!(last.contains("95\t94,item_94,940"));
    assert!(last.contains("101\t100,item_100,1000"));
}

/// End-to-end workflow: scaffold → find → edit → verify.
/// Simulates an agent creating a project, searching for TODOs, and fixing them.
#[tokio::test]
async fn scaffold_find_fix_workflow() {
    let dir = tempfile::tempdir().unwrap();

    let writer = FileWriteTool::new();
    let reader = FileReadTool::new();
    let editor = FileEditTool::new();
    let finder = FileFindTool::new();

    // Step 1: Scaffold project files with TODOs
    let main = "fn main() {\n    // TODO: add argument parsing\n    let config = load_config();\n    run(config);\n}\n";
    let config = "pub fn load_config() -> Config {\n    // TODO: read from file instead of hardcoded\n    Config {\n        host: \"localhost\".into(),\n        port: 8080,\n    }\n}\n";
    let server = "pub fn run(config: Config) {\n    // TODO: implement graceful shutdown\n    println!(\"Listening on {}:{}\", config.host, config.port);\n}\n";

    let root = dir.path().to_str().unwrap();
    for (name, content) in [("main.rs", main), ("config.rs", config), ("server.rs", server)] {
        let p = dir.path().join("src").join(name);
        writer
            .execute(json!({"path": p.to_str().unwrap(), "content": content}))
            .await
            .unwrap();
    }

    // Step 2: Find all TODOs
    let todos = finder
        .execute(json!({"path": root, "content": "TODO"}))
        .await
        .unwrap();
    assert!(todos.contains("main.rs"));
    assert!(todos.contains("config.rs"));
    assert!(todos.contains("server.rs"));

    // Step 3: Fix the config TODO
    let config_path = dir.path().join("src/config.rs");
    editor
        .execute(json!({
            "path": config_path.to_str().unwrap(),
            "old_string": "    // TODO: read from file instead of hardcoded\n    Config {\n        host: \"localhost\".into(),\n        port: 8080,\n    }",
            "new_string": "    let file = std::fs::read_to_string(\"config.toml\")?;\n    toml::from_str(&file)?"
        }))
        .await
        .unwrap();

    // Step 4: Verify fix applied and other TODOs remain
    let result = reader
        .execute(json!({"path": config_path.to_str().unwrap()}))
        .await
        .unwrap();
    assert!(result.contains("read_to_string"));
    assert!(!result.contains("TODO"));

    // Other files still have TODOs
    let remaining = finder
        .execute(json!({"path": root, "content": "TODO"}))
        .await
        .unwrap();
    assert!(remaining.contains("main.rs"));
    assert!(remaining.contains("server.rs"));
    assert!(!remaining.contains("config.rs")); // fixed!
}

/// Edge case: write + edit a file with Unicode and special characters.
#[tokio::test]
async fn unicode_and_special_chars() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("i18n.json");
    let path_str = path.to_str().unwrap();

    let writer = FileWriteTool::new();
    let reader = FileReadTool::new();
    let editor = FileEditTool::new();

    let content = "{\n  \"greeting\": \"你好世界\",\n  \"emoji\": \"🚀🎉\",\n  \"special\": \"<script>alert('xss')</script>\"\n}\n";
    writer
        .execute(json!({"path": path_str, "content": content}))
        .await
        .unwrap();

    // Edit Chinese text
    editor
        .execute(json!({
            "path": path_str,
            "old_string": "你好世界",
            "new_string": "こんにちは世界"
        }))
        .await
        .unwrap();

    let result = reader.execute(json!({"path": path_str})).await.unwrap();
    assert!(result.contains("こんにちは世界"));
    assert!(result.contains("🚀🎉"));
    assert!(!result.contains("你好世界"));
}

/// Deep directory creation: write files in deeply nested paths,
/// then find them with depth control.
#[tokio::test]
async fn deep_nested_find_with_depth() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();

    let writer = FileWriteTool::new();
    let finder = FileFindTool::new();

    // Create files at various depths
    let paths = vec![
        "level1.txt",
        "a/level2.txt",
        "a/b/level3.txt",
        "a/b/c/level4.txt",
        "a/b/c/d/level5.txt",
    ];
    for rel in &paths {
        let full = dir.path().join(rel);
        writer
            .execute(json!({"path": full.to_str().unwrap(), "content": "data"}))
            .await
            .unwrap();
    }

    // depth=0: only root-level files
    let result = finder
        .execute(json!({"path": root, "pattern": ".txt", "max_depth": 0}))
        .await
        .unwrap();
    assert!(result.contains("level1.txt"));
    assert!(!result.contains("level2.txt"));

    // depth=2: up to a/b/
    let result = finder
        .execute(json!({"path": root, "pattern": ".txt", "max_depth": 2}))
        .await
        .unwrap();
    assert!(result.contains("level1.txt"));
    assert!(result.contains("level2.txt"));
    assert!(result.contains("level3.txt"));
    assert!(!result.contains("level4.txt"));

    // depth=10 (default): all files
    let result = finder
        .execute(json!({"path": root, "pattern": ".txt"}))
        .await
        .unwrap();
    assert!(result.contains("level5.txt"));
}
