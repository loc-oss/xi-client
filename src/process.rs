use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::{Error, Result};

pub struct XiProcess {
    child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub path: PathBuf,
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

        Ok(Self { child, stdin, stdout, path })
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for XiProcess {
    fn drop(&mut self) {
        self.kill();
    }
}
