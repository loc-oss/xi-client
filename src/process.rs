use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::types::SpawnOptions;
use crate::{Error, Result};

/// Owns the `xi-core` subprocess and its stdio handles.
///
/// The child is killed on `Drop`. `stdin` and `stdout` are handed out exactly
/// once via [`take_stdin`] / [`take_stdout`]; after that the callers (the RPC
/// writer and reader respectively) own them.
pub struct XiProcess {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<ChildStdout>>,
    path: PathBuf,
}

impl XiProcess {
    /// Spawns with default options. Equivalent to
    /// `spawn_with_options(core_path, &SpawnOptions::default())`.
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self> {
        Self::spawn_with_options(core_path, &SpawnOptions::default())
    }

    pub fn spawn_with_options(core_path: impl AsRef<Path>, opts: &SpawnOptions) -> Result<Self> {
        let path = core_path.as_ref().to_path_buf();
        let mut cmd = Command::new(&path);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());

        if opts.capture_stderr {
            cmd.stderr(Stdio::piped());
        }

        if let Some(level) = &opts.log_level {
            cmd.env("XI_LOG", level);
        }
        if let Some(dir) = &opts.log_dir {
            cmd.arg("-log-dir").arg(dir);
        }
        if let Some(file) = &opts.log_file {
            cmd.arg("-log-file").arg(file);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Spawn(format!("{}: {e}", path.display())))?;

        // Drain stderr in a background thread if captured. This prevents the
        // pipe buffer from filling up and blocking xi-core. The bytes are
        // discarded; use `log_dir` / `log_file` to inspect logs.
        if let Some(mut stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("xi-client-stderr".into())
                .spawn(move || {
                    use std::io::Read;
                    let mut buf = [0u8; 8192];
                    loop {
                        match stderr.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                })
                .ok();
        }

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

    pub fn take_stdin(&self) -> Option<ChildStdin> {
        self.stdin.lock().unwrap().take()
    }

    pub fn take_stdout(&self) -> Option<ChildStdout> {
        self.stdout.lock().unwrap().take()
    }

    /// Returns `true` if the child has not yet exited. Non-blocking.
    pub fn is_running(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    /// Blocks until the child exits and reaps it.
    ///
    /// Call this only after the reader thread has observed EOF (i.e. after
    /// [`crate::XiClient::wait_for_exit`] has confirmed exit); otherwise it
    /// blocks until the child exits on its own.
    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        self.child.lock().unwrap().wait()
    }

    /// Kills the child and reaps it. Idempotent.
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
/// `XiClient::shutdown` also needs a way to close stdin (which is how
/// `xi-core` learns that it should exit — see `docs/protocol.md` §4.3). This
/// wrapper keeps the `ChildStdin` behind an `Arc<Mutex<Option<...>>>` so both
/// parties can hold a clone; [`close`] drops the inner handle, sending EOF.
#[derive(Clone)]
pub struct SharedStdin(Arc<Mutex<Option<ChildStdin>>>);

impl SharedStdin {
    pub fn new(stdin: ChildStdin) -> Self {
        Self(Arc::new(Mutex::new(Some(stdin))))
    }

    /// Closes stdin (sends EOF to `xi-core`). Idempotent.
    /// Returns `true` if this call actually closed it.
    pub fn close(&self) -> bool {
        self.0.lock().unwrap().take().is_some()
    }

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
            None => Ok(()),
        }
    }
}
