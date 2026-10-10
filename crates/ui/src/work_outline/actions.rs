//! Seam 3: the mutation facade over the `WorkOutlineMutate` RPC.
//!
//! The facade exposes one typed method per operation and returns a gpui
//! [`Task`], so views never touch the RPC client. Every call takes the wire
//! params struct, which carries the client-generated `operation_id`: callers
//! mint one id per logical user action with [`new_operation_id`] and reuse it
//! on every retry of that action, so retries replay idempotently. Optimistic
//! echo and rollback (the `persist_chat_title` pattern in `shell.rs`) and the
//! single in-flight guard per item belong to the data/shell owner, who
//! replaces the stub body of [`WorkOutlineActions::mutate`].
//!
//! Today every call fails with `Unsupported`: the engine handlers land after
//! the scaffold.
use gpui::Task;
use zeron_proto::{
    ArchiveWorkItemParams, AssignChatToWorkItemParams, CreateWorkItemExternalReferenceParams,
    CreateWorkItemParams, MoveWorkItemParams, RemoveChatFromWorkItemParams,
    RemoveWorkItemExternalReferenceParams, RepairWorkOutlineParams, RestoreWorkItemParams,
    UpdateWorkItemParams, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineMutateReply,
    WorkOutlineMutation,
};

use crate::state::EngineHandle;

/// A new idempotency key for one logical user action (UUID v4).
pub(crate) fn new_operation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Every operation resolves to the shared reply (`{}` unless the operation
/// mints an id) or a coded [`WorkOutlineError`].
pub(crate) type ActionTask = Task<Result<WorkOutlineMutateReply, WorkOutlineError>>;

/// Mutation facade. Cheap to clone; holds only the engine handle.
#[derive(Clone)]
pub(crate) struct WorkOutlineActions {
    engine: Option<EngineHandle>,
}

impl WorkOutlineActions {
    pub(crate) fn new(engine: Option<EngineHandle>) -> Self {
        Self { engine }
    }

    /// Send one mutation and decode the reply. STUB: owned by the data/shell
    /// owner (`state.rs` / `shell/work_outline_ui.rs` work); until then every
    /// operation reports `Unsupported`. Implementation notes: call
    /// `methods::WORK_OUTLINE_MUTATE` with the serialized mutation, map an
    /// error string through `WorkOutlineError::from_wire_message` (and an
    /// `unknown method` error to `Unsupported`), and decode the reply as
    /// `WorkOutlineMutateReply`.
    fn mutate(&self, _mutation: WorkOutlineMutation) -> ActionTask {
        let _ = &self.engine;
        Task::ready(Err(WorkOutlineError::new(
            WorkOutlineErrorCode::Unsupported,
            "Work Outline actions are not connected to the engine yet",
        )))
    }

    /// Reply carries `work_item_id` (`derive_id(operationId, "item")`);
    /// decode with `WorkOutlineMutateReply::into_create_work_item`.
    pub(crate) fn create_work_item(&self, params: CreateWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::CreateWorkItem(params))
    }

    /// Rename, retitle, change status, edit the description, or toggle
    /// instruction inheritance (`WorkItemChanges`).
    pub(crate) fn update_work_item(&self, params: UpdateWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::UpdateWorkItem(params))
    }

    /// Reparent and/or reorder (`MoveWorkItemParams::placement`).
    pub(crate) fn move_work_item(&self, params: MoveWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::MoveWorkItem(params))
    }

    pub(crate) fn archive_work_item(&self, params: ArchiveWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::ArchiveWorkItem(params))
    }

    pub(crate) fn restore_work_item(&self, params: RestoreWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::RestoreWorkItem(params))
    }

    /// Associate a Chat with a work item of the same Space.
    pub(crate) fn assign_chat(&self, params: AssignChatToWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::AssignChatToWorkItem(params))
    }

    /// Dissociate a Chat (writes an explicit disassociation, not a delete).
    pub(crate) fn remove_chat(&self, params: RemoveChatFromWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::RemoveChatFromWorkItem(params))
    }

    /// Reply carries `reference_id`; decode with
    /// `WorkOutlineMutateReply::into_create_external_reference`.
    pub(crate) fn create_external_reference(
        &self,
        params: CreateWorkItemExternalReferenceParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::CreateWorkItemExternalReference(params))
    }

    pub(crate) fn remove_external_reference(
        &self,
        params: RemoveWorkItemExternalReferenceParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::RemoveWorkItemExternalReference(params))
    }

    /// Persist derived repairs (cycle victims, bad order keys) for a Space.
    pub(crate) fn repair(&self, params: RepairWorkOutlineParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::RepairWorkOutline(params))
    }
}
