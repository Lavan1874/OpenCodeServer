use super::runtime_durability::RuntimePersistence;
use super::*;
use crate::runtime_state::LaunchPending;

impl Supervisor {
    /// Write the first phase of an OpenCode launch. No child creation is
    /// allowed until this marker is durably present, so a replacement
    /// OpenCodeServerAgent can distinguish "the old child may exist" from
    /// "there was no launch attempt".
    pub(super) fn begin_launch(&mut self, pending: LaunchPending) -> bool {
        if !self.runtime_state_loaded
            || !self.runtime_state_reliable
            || self.runtime.launch_pending.is_some()
            || self.launch_pending_clear_requested
        {
            return false;
        }
        let previous = self.runtime.launch_pending.replace(pending);
        match self.persist_runtime_detailed() {
            RuntimePersistence::Durable => true,
            RuntimePersistence::Failed => {
                self.runtime.launch_pending = previous;
                false
            }
            RuntimePersistence::Uncertain => {
                // The marker may already be visible after rename. Keep it
                // in memory and request an unwind after a retry proves the
                // marker write durable; spawning is still refused now.
                self.launch_pending_clear_requested = true;
                false
            }
        }
    }

    /// Commit an OpenCode child and clear the marker in one atomic runtime
    /// state write. This covers both confirmed starts and identity-
    /// confirmation survivors. On failure the Child remains in `Supervisor`,
    /// while the durable marker remains the cross-OpenCodeServerAgent evidence
    /// that a launch may already have created OpenCode.
    pub(super) fn commit_spawned_process(&mut self, process: ManagedProcess) -> bool {
        self.runtime.process = Some(process.record().clone());
        self.runtime.launch_pending = None;
        self.launch_pending_clear_requested = true;
        self.process = Some(process);
        self.persist_runtime()
    }

    /// Finish the pre-spawn transaction after a spawn attempt that produced
    /// no surviving child. Another launch cannot be attempted while the disk
    /// may still describe an unresolved launch.
    pub(super) fn abort_launch(&mut self) -> bool {
        if self.runtime.launch_pending.is_none() && !self.launch_pending_clear_requested {
            return true;
        }
        self.runtime.launch_pending = None;
        self.launch_pending_clear_requested = true;
        self.persist_runtime()
    }
}
