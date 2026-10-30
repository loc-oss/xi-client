use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde_json::{json, Value};

use crate::document::DocumentState;
use crate::events::Event;
use crate::process::XiProcess;
use crate::rpc::{RpcReader, RpcWriter};
use crate::types::{Config, Style, ViewId};
use crate::{Error, Result};

struct Inner {
    reader: RpcReader,
    writer: RpcWriter,
    docs: HashMap<ViewId, DocumentState>,
    styles: HashMap<u64, Style>,
    event_tx: Sender<Event>,
    next_view_request: u64,
}

pub struct XiClient {
    inner: Arc<Mutex<Inner>>,
    event_rx: Receiver<Event>,
    _process: XiProcess,
}

impl XiClient {
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self> {
        let mut process = XiProcess::spawn(core_path)?;
        let stdin = std::mem::replace(&mut process.stdin, {
            unimplemented!("заглушка")
        });

        let _ = stdin;
        let _ = process;
        unimplemented!("см. ветку ниже — пошаговая реализация")
    }

    pub fn new_view(&self, _path: Option<PathBuf>) -> Result<u64> {
        Err(Error::Rpc("not implemented".into()))
    }
    pub fn insert(&self, _view_id: ViewId, _chars: &str) {}
    pub fn delete_backward(&self, _view_id: ViewId) {}
    pub fn move_left(&self, _view_id: ViewId) {}
    pub fn move_right(&self, _view_id: ViewId) {}
    pub fn move_up(&self, _view_id: ViewId) {}
    pub fn move_down(&self, _view_id: ViewId) {}
    pub fn insert_newline(&self, _view_id: ViewId) {}
    pub fn close_view(&self, _view_id: ViewId) {}
    pub fn save(&self, _view_id: ViewId, _path: &Path) {}
    pub fn document(&self, _view_id: ViewId) -> Option<crate::DocumentSnapshot> { None }
    pub fn events(&self) -> Receiver<Event> { self.event_rx.clone() }
    pub fn shutdown(&self) {}
}

#[allow(dead_code)]
fn _keep_types(_: Config, _: Style, _: Value) { let _ = json!({}); }
