use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout};

use serde_json::Value;

use crate::{Error, Result};

pub struct RpcReader {
    reader: BufReader<ChildStdout>,
}

impl RpcReader {
    pub fn new(stdout: ChildStdout) -> Self {
        Self { reader: BufReader::new(stdout) }
    }

    pub fn next(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(trimmed)?;
        Ok(Some(v))
    }
}

pub struct RpcWriter {
    writer: ChildStdin,
    next_id: u64,
}

impl RpcWriter {
    pub fn new(stdin: ChildStdin) -> Self {
        Self { writer: stdin, next_id: 1 }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = serde_json::json!({ "method": method, "params": params });
        self.write_line(&msg)
    }

    pub fn request(&mut self, id: u64, method: &str, params: Value) -> Result<()> {
        let msg = serde_json::json!({ "id": id, "method": method, "params": params });
        self.write_line(&msg)
    }

    pub fn respond(&mut self, id: u64, result: Value) -> Result<()> {
        let msg = serde_json::json!({ "id": id, "result": result });
        self.write_line(&msg)
    }

    fn write_line(&mut self, v: &Value) -> Result<()> {
        let s = serde_json::to_string(v)?;
        self.writer.write_all(s.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

#[allow(dead_code)]
fn _touch_read_trait<R: Read>(_r: R) {}

#[allow(dead_code)]
pub fn spawn_reader_thread(mut reader: RpcReader, tx: crossbeam_channel::Sender<Value>) {
    std::thread::spawn(move || loop {
        match reader.next() {
            Ok(Some(v)) => {
                if tx.send(v).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    });
}
