/// Runs `command` to its end with `input` on its standard input; returns its success and its output (both streams).
#[allow(dead_code)]
pub async fn run(mut command: std::process::Command, input: &'static [u8]) -> (bool, String) {
    spawn_blocking(move || {
        let program = command.get_program().to_owned();
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                panic!("{} is required by this test: {error}", program.display())
            });
        // The tool may stop reading before the input is written; that is not what the test looks at.
        let _ = std::io::Write::write_all(&mut child.stdin.take().expect("piped stdin"), input);
        let output = child.wait_with_output().expect("tool output");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    })
    .await
    .expect("tool task")
}

#[allow(dead_code)]
pub fn curl(port: u16, flags: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("curl");
    command
        .args(["--silent", "--show-error", "--max-time", "15", "--cacert"])
        .arg(fixture("ca-a.pem"))
        .arg("--resolve")
        .arg(format!("{PROXY_SERVER_NAME}:{port}:127.0.0.1"))
        .args(["--write-out", "\n[%{http_code} http%{http_version}]"])
        .args(flags)
        .arg(format!("https://{PROXY_SERVER_NAME}:{port}/curl"));
    command
}
