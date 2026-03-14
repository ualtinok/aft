use std::cell::{Ref, RefCell, RefMut};

use crate::backup::BackupStore;
use crate::callgraph::CallGraph;
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::language::LanguageProvider;

/// Shared application context threaded through all command handlers.
///
/// Holds the language provider, backup/checkpoint stores, configuration,
/// and call graph engine. Constructed once at startup and passed by
/// reference to `dispatch`.
///
/// Stores use `RefCell` for interior mutability — the binary is single-threaded
/// (one request at a time on the stdin read loop) so runtime borrow checking
/// is safe and never contended.
pub struct AppContext {
    provider: Box<dyn LanguageProvider>,
    backup: RefCell<BackupStore>,
    checkpoint: RefCell<CheckpointStore>,
    config: RefCell<Config>,
    callgraph: RefCell<Option<CallGraph>>,
}

impl AppContext {
    pub fn new(provider: Box<dyn LanguageProvider>, config: Config) -> Self {
        AppContext {
            provider,
            backup: RefCell::new(BackupStore::new()),
            checkpoint: RefCell::new(CheckpointStore::new()),
            config: RefCell::new(config),
            callgraph: RefCell::new(None),
        }
    }

    /// Access the language provider.
    pub fn provider(&self) -> &dyn LanguageProvider {
        self.provider.as_ref()
    }

    /// Access the backup store.
    pub fn backup(&self) -> &RefCell<BackupStore> {
        &self.backup
    }

    /// Access the checkpoint store.
    pub fn checkpoint(&self) -> &RefCell<CheckpointStore> {
        &self.checkpoint
    }

    /// Access the configuration (shared borrow).
    pub fn config(&self) -> Ref<'_, Config> {
        self.config.borrow()
    }

    /// Access the configuration (mutable borrow).
    pub fn config_mut(&self) -> RefMut<'_, Config> {
        self.config.borrow_mut()
    }

    /// Access the call graph engine.
    pub fn callgraph(&self) -> &RefCell<Option<CallGraph>> {
        &self.callgraph
    }
}
