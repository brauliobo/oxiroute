use super::*;

#[test]
fn worker_command_encodes_exact_launcher_metadata_version() {
    let command = WorkerCommand::new("/bin/true")
        .unwrap()
        .arg("mode")
        .env("KEY", "value");
    let program = command.program.clone();
    let (_parent_endpoint, child_endpoint) = SeqpacketEndpoint::pair().unwrap();
    let launcher = command
        .into_launcher_command(
            Path::new("/bin/true"),
            child_endpoint,
            Some(Path::new("/cg/w")),
        )
        .unwrap();
    let encoded = launcher
        .get_args()
        .map(|argument| argument.as_bytes().to_vec())
        .collect::<Vec<_>>();

    assert_eq!(
        encoded,
        [
            program.as_os_str().as_bytes(),
            b"v2",
            b"1",
            b"2f63672f77",
            b"1",
            b"6d6f6465",
            b"1",
            b"4b4559",
            b"76616c7565",
        ]
        .map(<[u8]>::to_vec)
    );
}
