use std::{
    env, fs, io,
    io::{Read, Write},
    os::fd::{AsFd, OwnedFd},
    process::{Command, ExitCode, Stdio},
    thread,
    time::Duration,
};

use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::{FrameFlags, InstanceToken, MessageType, SeqpacketEndpoint};
use oxiroute_supervisor_process::{WorkerEndpoint, WorkerIdentity};
use rustix::{
    io::IoSlice,
    net::{SendAncillaryBuffer, SendFlags, sendmsg},
};

const INSTANCE: InstanceToken = InstanceToken(*b"process-worker01");
const PROTOCOL: u16 = 7;
const READY: MessageType = MessageType(0xff01);

fn identity() -> WorkerIdentity {
    WorkerIdentity {
        instance: INSTANCE,
        generation: GenerationId(11),
        protocol: PROTOCOL,
    }
}

fn adopt_stdin() -> Result<SeqpacketEndpoint, Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let owned: OwnedFd = rustix::io::fcntl_dupfd_cloexec(&stdin, 0)?;
    Ok(SeqpacketEndpoint::from_owned_fd(owned)?)
}

fn forged_ready(mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut endpoint = adopt_stdin()?;
    let challenge = endpoint.receive()?;
    let mut payload = challenge.payload().to_vec();
    let mut instance = INSTANCE;
    let mut generation = GenerationId(11);
    match mode {
        "wrong-nonce" => payload[2] ^= 1,
        "wrong-generation" => generation = GenerationId(12),
        "wrong-protocol" => payload[..2].copy_from_slice(&(PROTOCOL + 1).to_be_bytes()),
        "legacy-v1" => payload[..2].copy_from_slice(&1_u16.to_be_bytes()),
        "wrong-instance" => instance = InstanceToken(*b"different-worker"),
        _ => {}
    }
    endpoint.send(
        READY,
        FrameFlags::default(),
        instance,
        generation,
        &payload,
        &[],
    )?;
    Ok(())
}

fn post_ready_grandchild_sender() -> Result<(), Box<dyn std::error::Error>> {
    let mut endpoint = adopt_stdin()?;
    let challenge = endpoint.receive()?;
    endpoint.send(
        READY,
        FrameFlags::default(),
        INSTANCE,
        GenerationId(11),
        challenge.payload(),
        &[],
    )?;

    let endpoint_copy = rustix::io::fcntl_dupfd_cloexec(&endpoint, 0)?;
    let (mut nonce_writer, nonce_reader) = std::os::unix::net::UnixStream::pair()?;
    nonce_writer.write_all(&challenge.payload()[2..34])?;
    drop(nonce_writer);
    Command::new(env::current_exe()?)
        .arg("grandchild-send")
        .stdin(Stdio::from(OwnedFd::from(nonce_reader)))
        .stdout(Stdio::from(endpoint_copy))
        .spawn()?;
    thread::sleep(Duration::from_secs(30));
    Ok(())
}

fn raw_grandchild_send() -> Result<(), Box<dyn std::error::Error>> {
    let mut nonce = [0_u8; 32];
    io::stdin().read_exact(&mut nonce)?;
    let application = b"from grandchild";
    let mut frame = vec![0_u8; 52 + 34 + application.len()];
    frame[0..4].copy_from_slice(b"OXSP");
    frame[4..6].copy_from_slice(&1_u16.to_be_bytes());
    frame[6..8].copy_from_slice(&200_u16.to_be_bytes());
    frame[8..12].copy_from_slice(&u32::try_from(34 + application.len())?.to_be_bytes());
    frame[20..28].copy_from_slice(&2_u64.to_be_bytes());
    frame[28..44].copy_from_slice(&INSTANCE.0);
    frame[44..52].copy_from_slice(&11_u64.to_be_bytes());
    frame[52..54].copy_from_slice(&PROTOCOL.to_be_bytes());
    frame[54..86].copy_from_slice(&nonce);
    frame[86..].copy_from_slice(application);
    let mut ancillary = SendAncillaryBuffer::default();
    sendmsg(
        io::stdout().as_fd(),
        &[IoSlice::new(&frame)],
        &mut ancillary,
        SendFlags::NOSIGNAL,
    )?;
    Ok(())
}

fn environment_mode() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let _endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
    let valid = env::var_os("PATH").is_none()
        && env::var("CONFIGURED_MODE").as_deref() == Ok("fixture")
        && env::var("LD_PRELOAD").as_deref() == Ok("/definitely/not/a/preload-library.so")
        && env::var_os("OXIROUTE_AUDIT_DIR").is_some()
        && env::var("OXIROUTE_AUDIT_MAX_FILE_BYTES").as_deref() == Ok("1048576")
        && env::var_os("HOME").is_some();
    Ok(if valid {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(91)
    })
}

fn sentinel_mode() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let _endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
    let sentinel = env::var_os("SENTINEL_TARGET").ok_or("missing sentinel target")?;
    let leaked = fs::read_dir("/proc/self/fd")?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| fs::read_link(entry.path()).ok())
            .is_some_and(|target| target == sentinel)
    });
    Ok(if leaked {
        ExitCode::from(92)
    } else {
        ExitCode::SUCCESS
    })
}

fn descendant_mode(linger: bool) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let _endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
    let child = Command::new(env::current_exe()?)
        .arg("descendant-sleep")
        .spawn()?;
    fs::write(
        env::var_os("DESCENDANT_PID_FILE").ok_or("missing pid file")?,
        child.id().to_string(),
    )?;
    if linger {
        thread::sleep(Duration::from_secs(30));
    }
    Ok(ExitCode::SUCCESS)
}

fn escaped_descendant_mode() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let _endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
    let child = Command::new(env::current_exe()?)
        .arg("escaped-descendant-sleep")
        .spawn()?;
    fs::write(
        env::var_os("DESCENDANT_PID_FILE").ok_or("missing pid file")?,
        child.id().to_string(),
    )?;
    if env::var_os("LINGER_AFTER_DESCENDANT").is_some() {
        thread::sleep(Duration::from_secs(30));
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mode = env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "success" => {
            let _endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
            thread::sleep(Duration::from_millis(200));
            Ok(ExitCode::SUCCESS)
        }
        "message" => {
            let mut endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
            endpoint.send(
                MessageType(50),
                FrameFlags::default(),
                b"authenticated payload",
                &[],
            )?;
            thread::sleep(Duration::from_secs(30));
            Ok(ExitCode::SUCCESS)
        }
        "environment" => environment_mode(),
        "sentinel" => sentinel_mode(),
        "descendant" => descendant_mode(true),
        "descendant-exit" => descendant_mode(false),
        "escaped-descendant-exit" => escaped_descendant_mode(),
        "linger" => {
            let _endpoint = WorkerEndpoint::adopt_at_process_entry(identity())?;
            thread::sleep(Duration::from_secs(30));
            Ok(ExitCode::SUCCESS)
        }
        "post-ready-grandchild" => {
            post_ready_grandchild_sender()?;
            Ok(ExitCode::SUCCESS)
        }
        "grandchild-send" => {
            raw_grandchild_send()?;
            Ok(ExitCode::SUCCESS)
        }
        "wrong-nonce" | "wrong-generation" | "wrong-protocol" | "legacy-v1" | "wrong-instance" => {
            forged_ready(&mode)?;
            thread::sleep(Duration::from_secs(30));
            Ok(ExitCode::SUCCESS)
        }
        "escaped-descendant-sleep" => {
            rustix::process::setsid()?;
            thread::sleep(Duration::from_secs(30));
            Ok(ExitCode::SUCCESS)
        }
        "descendant-sleep" | "timeout" => {
            thread::sleep(Duration::from_secs(30));
            Ok(ExitCode::SUCCESS)
        }
        "early-exit" => Ok(ExitCode::from(23)),
        "crash" => std::process::abort(),
        "credential-mismatch" => {
            let stdin = io::stdin();
            let endpoint = rustix::io::fcntl_dupfd_cloexec(&stdin, 0)?;
            let status = Command::new(env::current_exe()?)
                .arg("credential-sender")
                .stdin(Stdio::from(endpoint))
                .status()?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        "credential-sender" => {
            forged_ready("valid")?;
            Ok(ExitCode::SUCCESS)
        }
        _ => Ok(ExitCode::from(64)),
    }
}
