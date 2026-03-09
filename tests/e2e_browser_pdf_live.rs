//! Live end-to-end browser/PDF test.
//!
//! Runs the real agent loop with a real LLM provider from environment
//! configuration, uses the built-in `browser` tool, auto-starts the managed
//! browser runtime, opens a local HTML file, and verifies that a valid PDF is
//! produced.
//!
//! Usage:
//!   cargo test --test e2e_browser_pdf_live -- --ignored --nocapture

#[cfg(feature = "libsql")]
mod support;

#[cfg(feature = "libsql")]
mod live_tests {
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use ironclaw::config::Config;
    use ironclaw::llm::{create_llm_provider, create_session_manager};
    use tokio::process::{Child, Command};

    use crate::support::test_rig::TestRigBuilder;

    const TIMEOUT: Duration = Duration::from_secs(90);
    const TEST_PORT: &str = "24244";
    const NODE_TEST_PORT: &str = "24245";

    #[tokio::test]
    #[ignore = "requires live LLM credentials plus browser-runtime npm/playwright setup"]
    async fn live_llm_can_generate_pdf_via_browser_tool() {
        let _ = dotenvy::dotenv();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("ironclaw=info")
            .try_init();

        let html_path = write_local_html_fixture();
        let file_url = path_to_file_url(&html_path);

        unsafe {
            std::env::remove_var("IRONCLAW_BROWSER_BASE_URL");
            std::env::set_var("IRONCLAW_BROWSER_RUNTIME_MANAGED", "1");
            std::env::set_var("IRONCLAW_BROWSER_RUNTIME_PORT", TEST_PORT);
            std::env::set_var("IRONCLAW_BROWSER_RUNTIME_HEADLESS", "1");
        }

        let config = Config::from_env()
            .await
            .expect("live test requires LLM config in environment");
        let session = create_session_manager(config.llm.session.clone()).await;
        let llm = create_llm_provider(&config.llm, session)
            .expect("failed to create live LLM provider from environment");
        let llm: Arc<dyn ironclaw::llm::LlmProvider> = llm;

        let rig = TestRigBuilder::new().with_llm(llm).build().await;

        let prompt = format!(
            "Use the browser tool to open this local HTML page: {file_url}\n\
             Then generate a PDF from that page using the browser tool.\n\
             When finished, reply with exactly one line in this format:\n\
             PDF_PATH=<absolute path to the generated pdf>\n\
             Do not include any other text."
        );

        rig.send_message(&prompt).await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;

        assert!(!responses.is_empty(), "agent did not return a response");
        let final_text = &responses
            .last()
            .expect("response list should be non-empty")
            .content;

        let pdf_path = extract_pdf_path(final_text)
            .or_else(|| extract_pdf_path_from_tool_results(&rig.tool_results()))
            .expect("agent did not return a PDF path and no browser tool result exposed one");

        println!("LIVE_BROWSER_PDF_PATH={}", pdf_path.display());
        println!("LIVE_BROWSER_FINAL_RESPONSE={final_text}");
        for (name, preview) in rig.tool_results() {
            if name == "browser" {
                println!("LIVE_BROWSER_TOOL_RESULT={preview}");
            }
        }

        let pdf_bytes = std::fs::read(&pdf_path)
            .unwrap_or_else(|e| panic!("expected generated PDF at {}: {e}", pdf_path.display()));
        assert!(
            pdf_bytes.starts_with(b"%PDF-"),
            "generated file is not a valid PDF header: {}",
            pdf_path.display()
        );
        assert!(
            pdf_bytes.len() > 1024,
            "generated PDF is unexpectedly small: {} bytes",
            pdf_bytes.len()
        );
        println!("LIVE_BROWSER_PDF_BYTES={}", pdf_bytes.len());

        let started = rig.tool_calls_started();
        assert!(
            started.iter().any(|name| name == "browser"),
            "expected the browser tool to be used, saw {started:?}"
        );

        rig.shutdown();

        let _ = std::fs::remove_file(html_path);
        unsafe {
            std::env::remove_var("IRONCLAW_BROWSER_RUNTIME_PORT");
        }
    }

    #[tokio::test]
    #[ignore = "requires live LLM credentials plus browser-runtime npm/playwright setup"]
    async fn live_llm_can_generate_pdf_via_node_browser_proxy() {
        let _ = dotenvy::dotenv();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("ironclaw=info")
            .try_init();

        let html_path = write_local_html_fixture();
        let file_url = path_to_file_url(&html_path);
        let node_runtime = start_external_browser_runtime(NODE_TEST_PORT).await;
        let node_base_url = format!("http://127.0.0.1:{NODE_TEST_PORT}");

        unsafe {
            std::env::remove_var("IRONCLAW_BROWSER_BASE_URL");
            std::env::set_var("IRONCLAW_BROWSER_RUNTIME_MANAGED", "0");
            std::env::set_var("IRONCLAW_BROWSER_NODE_BASE_URL", &node_base_url);
        }

        let config = Config::from_env()
            .await
            .expect("live test requires LLM config in environment");
        let session = create_session_manager(config.llm.session.clone()).await;
        let llm = create_llm_provider(&config.llm, session)
            .expect("failed to create live LLM provider from environment");
        let llm: Arc<dyn ironclaw::llm::LlmProvider> = llm;

        let rig = TestRigBuilder::new().with_llm(llm).build().await;

        let prompt = format!(
            "Use the browser tool with target=node to open this local HTML page: {file_url}\n\
             Then generate a PDF from that page using the browser tool, still with target=node.\n\
             When finished, reply with exactly one line in this format:\n\
             PDF_PATH=<absolute path to the generated pdf>\n\
             Do not include any other text."
        );

        rig.send_message(&prompt).await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;

        assert!(!responses.is_empty(), "agent did not return a response");
        let final_text = &responses
            .last()
            .expect("response list should be non-empty")
            .content;

        let pdf_path = extract_pdf_path(final_text)
            .or_else(|| extract_pdf_path_from_tool_results(&rig.tool_results()))
            .expect("agent did not return a PDF path and no browser tool result exposed one");

        println!("LIVE_NODE_BROWSER_PDF_PATH={}", pdf_path.display());
        println!("LIVE_NODE_BROWSER_FINAL_RESPONSE={final_text}");
        for (name, preview) in rig.tool_results() {
            if name == "browser" {
                println!("LIVE_NODE_BROWSER_TOOL_RESULT={preview}");
            }
        }

        let pdf_bytes = std::fs::read(&pdf_path)
            .unwrap_or_else(|e| panic!("expected generated PDF at {}: {e}", pdf_path.display()));
        assert!(
            pdf_bytes.starts_with(b"%PDF-"),
            "generated file is not a valid PDF header: {}",
            pdf_path.display()
        );
        assert!(
            pdf_bytes.len() > 1024,
            "generated PDF is unexpectedly small: {} bytes",
            pdf_bytes.len()
        );
        println!("LIVE_NODE_BROWSER_PDF_BYTES={}", pdf_bytes.len());

        let started = rig.tool_calls_started();
        assert!(
            started.iter().any(|name| name == "browser"),
            "expected the browser tool to be used, saw {started:?}"
        );

        rig.shutdown();
        stop_external_browser_runtime(node_runtime).await;

        let _ = std::fs::remove_file(html_path);
        unsafe {
            std::env::remove_var("IRONCLAW_BROWSER_RUNTIME_MANAGED");
            std::env::remove_var("IRONCLAW_BROWSER_NODE_BASE_URL");
        }
    }

    fn write_local_html_fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ironclaw-live-browser-test-{}.html",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <title>IronClaw Live Browser Test</title>
  <style>
    body { font-family: -apple-system, BlinkMacSystemFont, sans-serif; margin: 48px; }
    h1 { color: #0f172a; }
    p { font-size: 16px; line-height: 1.6; }
    table { border-collapse: collapse; width: 100%; margin-top: 24px; }
    td, th { border: 1px solid #cbd5e1; padding: 8px; text-align: left; }
  </style>
</head>
<body>
  <h1>IronClaw Live Browser Test</h1>
  <p>这份 HTML 用来验证真实大模型是否能调用 browser 工具并成功导出 PDF。</p>
  <table>
    <tr><th>检查项</th><th>期望</th></tr>
    <tr><td>open</td><td>页面可以打开</td></tr>
    <tr><td>pdf</td><td>生成有效 PDF 文件</td></tr>
  </table>
</body>
</html>
"#,
        )
        .expect("failed to write local browser test html");
        path
    }

    fn path_to_file_url(path: &std::path::Path) -> String {
        url::Url::from_file_path(path)
            .expect("temp html path should convert to file:// URL")
            .to_string()
    }

    fn extract_pdf_path(text: &str) -> Option<PathBuf> {
        if let Some(line) = text.lines().find(|line| line.starts_with("PDF_PATH=")) {
            let path = line.trim_start_matches("PDF_PATH=").trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }

        text.split_whitespace()
            .find(|token| token.ends_with(".pdf") && token.starts_with('/'))
            .map(|token| {
                PathBuf::from(
                    token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ')' | '(')),
                )
            })
    }

    fn extract_pdf_path_from_tool_results(results: &[(String, String)]) -> Option<PathBuf> {
        results
            .iter()
            .filter(|(name, _)| name == "browser")
            .find_map(|(_, preview)| extract_pdf_path(preview))
    }

    async fn start_external_browser_runtime(port: &str) -> Child {
        let runtime_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("browser-runtime");
        let runtime_script = runtime_dir.join("server.mjs");
        let mut child = Command::new("node")
            .arg(runtime_script)
            .current_dir(&runtime_dir)
            .env("IRONCLAW_BROWSER_RUNTIME_HOST", "127.0.0.1")
            .env("IRONCLAW_BROWSER_RUNTIME_PORT", port)
            .env("IRONCLAW_BROWSER_RUNTIME_HEADLESS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn external browser runtime");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("failed to build probe client");
        let base_url = format!("http://127.0.0.1:{port}");
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if let Ok(resp) = client.get(format!("{base_url}/")).send().await
                && resp.status().is_success()
            {
                return child;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("external browser runtime exited early with status {status}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let _ = child.start_kill();
        panic!("external browser runtime did not become ready at {base_url}");
    }

    async fn stop_external_browser_runtime(mut child: Child) {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}
