//! proc — every subprocess fleetd waits on runs here, under a deadline. One that outlives it is
//! killed with its whole process group (a `git fetch` and the `ssh` it started), so no hung call
//! holds the scheduler's one loop past its deadline.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A forge or tracker CLI: `glab`, `gh`, `jira`, `curl`.
pub const API: Duration = Duration::from_secs(60);
/// git: a fetch, or a worktree cut from a large checkout.
pub const GIT: Duration = Duration::from_secs(120);
/// tmux.
pub const LOCAL: Duration = Duration::from_secs(15);

const POLL: Duration = Duration::from_millis(20);
/// How long a pipe may stay open once its holders are killed (one outside the group may hold it).
const GRACE: Duration = Duration::from_secs(1);

/// `Command::output`, under a deadline.
pub trait OutputWithin {
    fn output_within(&mut self, deadline: Duration) -> io::Result<Output>;
}

impl OutputWithin for Command {
    fn output_within(&mut self, deadline: Duration) -> io::Result<Output> {
        run(self, None, deadline)
    }
}

/// Run `cmd` to its end within `deadline`: `input` on its stdin (else none), its output captured.
/// Past the deadline its process group is killed and the call fails `TimedOut`. A process it left
/// behind holding its pipes (the command itself done) is killed at the deadline too.
pub fn run(cmd: &mut Command, input: Option<&[u8]>, deadline: Duration) -> io::Result<Output> {
    let until = Instant::now() + deadline;
    let mut child = cmd
        .process_group(0)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let group = child.id() as libc::pid_t;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(bytes)?; // dropped here: EOF
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= until {
            kill(group);
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("timed out after {}s", deadline.as_secs())));
        }
        std::thread::sleep(POLL);
    };
    let collect = |pipe: Receiver<Vec<u8>>| {
        pipe.recv_timeout(until.saturating_duration_since(Instant::now()))
            .or_else(|_| {
                kill(group);
                pipe.recv_timeout(GRACE)
            })
            .unwrap_or_default()
    };
    Ok(Output { status, stdout: collect(stdout), stderr: collect(stderr) })
}

/// Read a pipe to its end on its own thread; the bytes arrive on the receiver.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// SIGKILL every process left in the group (none left: a no-op).
fn kill(group: libc::pid_t) {
    // SAFETY: killpg only signals; the group is the one this call spawned.
    unsafe { libc::killpg(group, libc::SIGKILL) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, deadline: Duration) -> (io::Result<Output>, Duration) {
        let start = Instant::now();
        (Command::new("sh").args(["-c", script]).output_within(deadline), start.elapsed())
    }

    #[test]
    fn a_quick_command_returns_its_output() {
        let (out, _) = sh("echo hi; echo oops >&2; exit 3", API);
        let out = out.unwrap();
        assert_eq!((out.stdout.as_slice(), out.stderr.as_slice(), out.status.code()), (&b"hi\n"[..], &b"oops\n"[..], Some(3)));
    }

    #[test]
    fn a_hung_command_and_its_children_are_killed_at_the_deadline() {
        let (out, took) = sh("sleep 30 & wait", Duration::from_millis(300));
        assert_eq!(out.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    #[test]
    fn a_child_left_holding_the_pipes_is_killed_and_the_output_kept() {
        // The incident's shape: the command is done, but a process it started (git's ssh) holds
        // its stderr open, so reading to EOF would wait forever.
        let (out, took) = sh("sleep 30 & echo hi", Duration::from_millis(300));
        let out = out.unwrap();
        assert_eq!((out.stdout.as_slice(), out.status.success()), (&b"hi\n"[..], true));
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    #[test]
    fn input_goes_to_its_stdin() {
        let out = run(Command::new("cat").arg("-"), Some(b"user = x"), API).unwrap();
        assert_eq!(out.stdout, b"user = x");
    }
}
