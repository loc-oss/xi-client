use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::{Error, Result};

/// Owns the `xi-core` subprocess and its stdio handles.
///
/// The child is killed on `Drop`. `stdin` and `stdout` are handed out exactly
/// once via [`take_stdin`] / [`take_stdout`]; after that the callers (the RPC
/// writer and reader respectively) own them.
///
/// `child` is behind a `Mutex` so that `kill`/`is_running` can be called from
/// `&self` — which is what `XiClient::shutdown` needs.
pub struct XiProcess {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<ChildStdout>>,
    path: PathBuf,
}

impl XiProcess {
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self> {
        let path = core_path.as_ref().to_path_buf();
        let mut child = Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Spawn(format!("{}: {e}", path.display())))?;

        let stdin = child.stdin.take().ok_or_else(|| Error::Spawn("нет stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Spawn("нет stdout".into()))?;

        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(Some(stdout)),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Moves `ChildStdin` out of this process. Intended to be called exactly
    /// once, during `XiClient::spawn`. Further calls return `None`.
    pub fn take_stdin(&self) -> Option<ChildStdin> {
        self.stdin.lock().unwrap().take()
    }

    /// Moves `ChildStdout` out of this process. Intended to be called exactly
    /// once, during `XiClient::spawn`. Further calls return `None`.
    pub fn take_stdout(&self) -> Option<ChildStdout> {
        self.stdout.lock().unwrap().take()
    }

    /// Returns `true` if the child has not yet exited.
    ///
    /// Non-blocking; reaps the child if it has exited.
    pub fn is_running(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    /// Kills the child (SIGKILL) and reaps it. Idempotent.
    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for XiProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Shared, closable handle to `xi-core`'s stdin.
///
/// `RpcLoop` requires `Write + Send` and takes ownership of the writer, but
/// `XiClient::shutdown` also needs a way to close stdin (which is how `xi-core`
/// learns that it should exit — see `docs/protocol.md` §4.3). This wrapper
/// keeps the `ChildStdin` behind an `Arc<Mutex<Option<...>>>` so both parties
/// can hold a clone; [`close`] drops the inner handle, sending EOF to the child.
///
/// After `close`, all writes fail with `ErrorKind::BrokenPipe`. `RpcLoop`'s
/// `send_rpc_notification` logs and swallows those errors, which is exactly
/// what we want during shutdown.
#[derive(Clone)]
pub struct SharedStdin(Arc<Mutex<Option<ChildStdin>>>);

impl SharedStdin {
    pub fn new(stdin: ChildStdin) -> Self {
        Self(Arc::new(Mutex::new(Some(stdin))))
    }

    /// Closes stdin (sends EOF to `xi-core`). Idempotent.
    /// Returns `true` if this call actually closed it, `false` if it was
    /// already closed.
    pub fn close(&self) -> bool {
        self.0.lock().unwrap().take().is_some()
    }

    /// Returns `true` if stdin has not been closed yet.
    pub fn is_open(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

impl Write for SharedStdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.0.lock().unwrap();
        match guard.as_mut() {
            Some(stdin) => stdin.write(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "xi-core stdin is closed",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.0.lock().unwrap();
        match guard.as_mut() {
            Some(stdin) => stdin.flush(),
            // Nothing to flush on a closed pipe; don't surface an error.
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_and_kill_roundtrip() {
        // Uses `sh -c 'sleep 60'` as a cheap stand-in for xi-core.
        let process = XiProcess::spawn("sh").unwrap();
        // sh is running, but we spawned it without args — it'll exit on EOF.
        // Instead of relying on sh's behavior, just exercise the accessors.
        assert_eq!(process.path(), Path::new("sh"));
        assert!(process.take_stdin().is_some());
        assert!(process.take_stdin().is_none(), "take_stdin is one-shot");
        assert!(process.take_stdout().is_some());
        assert!(process.take_stdout().is_none(), "take_stdout is one-shot");
        process.kill();
        assert!(!process.is_running());
    }

    #[test]
    fn shared_stdin_close_is_idempotent() {
        let process = XiProcess::spawn("sh").unwrap();
        let stdin = process.take_stdin().unwrap();
        let shared = SharedStdin::new(stdin);
        assert!(shared.is_open());
        assert!(shared.close());
        assert!(!shared.is_open());
        assert!(!shared.close(), "second close is a no-op");
        process.kill();
    }

    #[test]
    fn shared_stdin_write_after_close_fails() {
        let process = XiProcess::spawn("sh").unwrap();
        let stdin = process.take_stdin().unwrap();
        let mut shared = SharedStdin::new(stdin);
        shared.close();
        let err = shared.write(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        // flush is forgiving:
        assert!(shared.flush().is_ok());
        process.kill();
    }
}
