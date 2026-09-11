//! Embedding execution ownership. The host owns cancellation and subprocesses;
//! standalone CLI calls use ordinary synchronous commands without host state.
use std::process::{Command, Output};
use std::sync::Arc;

pub trait ExecutionHost: Send + Sync {
    fn cancelled(&self) -> bool;
    fn command(&self, command: Command) -> std::io::Result<Output>;
}

#[derive(Clone, Default)]
pub struct ExecutionControl(Option<Arc<dyn ExecutionHost>>);
impl ExecutionControl {
    pub fn new(host: Arc<dyn ExecutionHost>) -> Self {
        Self(Some(host))
    }
    pub fn check(&self) -> Result<(), String> {
        if self.0.as_ref().is_some_and(|host| host.cancelled()) {
            Err("Search cancelled by its execution owner".into())
        } else {
            Ok(())
        }
    }
    pub fn command(&self, mut command: Command) -> std::io::Result<Output> {
        self.check().map_err(std::io::Error::other)?;
        match &self.0 {
            Some(host) => host.command(command),
            None => command.output(),
        }
    }
}
