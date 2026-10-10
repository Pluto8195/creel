//! Seam between Work Outline registry state and runtime instruction delivery.
//!
//! The registry side (`WorkspaceHost`) implements this trait; the delivery side
//! (command executor, sessions, run journal) consumes it. Delivery never reads
//! outline rows directly, and the registry never knows about harness processes.

use zeron_proto::{ExecutionSessionReport, InstructionRevisionPin, InstructionRevisionView};

pub trait InstructionRegistry: Send + Sync {
    /// The exact revision by id, if it has synced to this device. Callers verify
    /// integrity against the pinned hash before delivery and never substitute
    /// another revision when this returns `None` (m0-contract §5.3).
    fn instruction_revision(&self, revision_id: &str) -> Option<InstructionRevisionView>;

    /// The Chat's selected revision: the latest accepted application by
    /// `(appliedHlc, id)` (m0-contract §5.6). `None` when the Chat has no
    /// accepted application.
    fn selected_instruction_revision(&self, chat_id: &str) -> Option<InstructionRevisionPin>;

    /// Records what an Execution Session actually received, on the host-written
    /// session-status row (m0-contract §5.6). Idempotent for an equal report.
    fn report_execution_session(&self, chat_id: &str, report: ExecutionSessionReport);
}
