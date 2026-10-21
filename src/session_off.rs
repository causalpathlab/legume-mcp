//! Without session jobs (feature `sessions`, on Unix): the server keeps no
//! state for them, and has none to take up.

use crate::jobs::Jobs;
use crate::watch::{Feed, Steer};
use crate::ServerInfo;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub(crate) struct Server;

impl Server {
    pub(crate) fn new(
        _: &ServerInfo,
        _: Option<&Path>,
        _: Option<Arc<Feed>>,
        _: Arc<Mutex<Steer>>,
    ) -> Self {
        Self
    }

    pub(crate) fn end(&self) {}

    pub(crate) fn adopt(&self, _: &Jobs, _: Option<&Path>, _: bool) {}
}
