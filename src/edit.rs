//! Reversible document commands and per-document undo/redo history.
//!
//! Commands retain immutable before/after documents so redo is deterministic
//! (including copied names and embedded definitions). History advances only
//! after the receiver successfully installs a command's state.
use crate::{document::Document, geometry::Point};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone)]
pub struct EditState {
    pub document: Arc<Document>,
    /// Top-level record indices, independent of scene instance numbering.
    pub selection: BTreeSet<usize>,
}

#[derive(Clone, Copy)]
pub struct Placement {
    pub pivot: Point,
    pub at: Point,
    pub rotation: u8,
    pub flip: bool,
}
impl Placement {
    fn apply(self, state: &mut EditState) -> Result<(), String> {
        if self.pivot != self.at || !self.rotation.is_multiple_of(4) || self.flip {
            state.document = Arc::new(state.document.with_transformed_records(
                &state.selection,
                self.pivot,
                self.at,
                self.rotation,
                self.flip,
            )?);
        }
        Ok(())
    }
}

pub enum EditOperation {
    Insert(EditState),
    InsertPrimitive(EditState),
    Move(Placement),
    Copy(Placement),
    Delete,
    Wire { start: Point, end: Point },
    WireRoute { start: Point, bend: Point, end: Point },
    SetInstanceProperty { key: String, value: String },
    SetRecordProperty { key: String, value: String },
    SetGraphConfiguration { records: BTreeSet<usize>, properties: std::collections::BTreeMap<String, String> },
}

/// A command prepares its result; the receiver installs it before history changes.
pub trait EditCommand {
    fn label(&self) -> &'static str;
    fn execute(&mut self) -> Result<&EditState, String>;
    fn undo(&self) -> &EditState;
}

pub struct DocumentCommand {
    operation: EditOperation,
    before: EditState,
    after: Option<EditState>,
}
impl DocumentCommand {
    pub fn new(operation: EditOperation, before: EditState) -> Self {
        Self {
            operation,
            before,
            after: None,
        }
    }
}
impl EditCommand for DocumentCommand {
    fn label(&self) -> &'static str {
        match self.operation {
            EditOperation::Insert(_) => "Place instance",
            EditOperation::InsertPrimitive(_) => "Place symbol primitive",
            EditOperation::SetGraphConfiguration { .. } => "Edit graph",
            EditOperation::Move(_) => "Move",
            EditOperation::Copy(_) => "Copy",
            EditOperation::Delete => "Delete",
            EditOperation::Wire { .. } | EditOperation::WireRoute { .. } => "Add wire",
            EditOperation::SetInstanceProperty { .. }
            | EditOperation::SetRecordProperty { .. } => "Edit property",
        }
    }
    fn execute(&mut self) -> Result<&EditState, String> {
        if self.after.is_none() {
            let mut state = self.before.clone();
            match self.operation {
                EditOperation::Insert(ref after) | EditOperation::InsertPrimitive(ref after) => state = after.clone(),
                EditOperation::SetGraphConfiguration { ref records, ref properties } => {
                    for (key, value) in properties {
                        state.document = Arc::new(state.document.with_record_property(records, key, value)?);
                    }
                }
                EditOperation::Move(placement) => placement.apply(&mut state)?,
                EditOperation::Copy(placement) => {
                    if !state.selection.is_empty() {
                        let (document, copied) =
                            state.document.with_copied_records(&state.selection)?;
                        state.document = Arc::new(document);
                        state.selection = copied;
                        placement.apply(&mut state)?;
                    }
                }
                EditOperation::Delete => {
                    state.document = Arc::new(state.document.without_records(&state.selection)?);
                    state.selection.clear();
                }
                EditOperation::WireRoute { start, bend, end } => {
                    for (a, b) in [(start, bend), (bend, end)] {
                        if a != b {
                            state.document = Arc::new(state.document.with_wire_unmerged(a, b)?);
                        }
                    }
                }
                EditOperation::Wire { start, end } => {
                    state.document = Arc::new(state.document.with_wire_unmerged(start, end)?);
                }
                EditOperation::SetRecordProperty { ref key, ref value } => {
                    state.document = Arc::new(state.document.with_record_property(
                        &state.selection,
                        key,
                        value,
                    )?);
                }
                EditOperation::SetInstanceProperty { ref key, ref value } => {
                    state.document = Arc::new(state.document.with_instance_property(
                        &state.selection,
                        key,
                        value,
                    )?);
                }
            }
            if state.document.original() != self.before.document.original() {
                let (document, selection) = state.document.normalize_wires_with_selection(&state.selection)?;
                state.document = Arc::new(document);
                state.selection = selection;
            }
            self.after = Some(state);
        }
        Ok(self.after.as_ref().unwrap())
    }
    fn undo(&self) -> &EditState {
        &self.before
    }
}

#[derive(Default)]
pub struct CommandHistory {
    done: Vec<Box<dyn EditCommand>>,
    undone: Vec<Box<dyn EditCommand>>,
}
impl CommandHistory {
    pub fn undo_label(&self) -> Option<&'static str> {
        self.done.last().map(|command| command.label())
    }
    pub fn redo_label(&self) -> Option<&'static str> {
        self.undone.last().map(|command| command.label())
    }
    pub fn execute(
        &mut self,
        mut command: Box<dyn EditCommand>,
        apply: impl FnOnce(&EditState) -> Result<(), String>,
    ) -> Result<(), String> {
        let before = Arc::clone(&command.undo().document);
        let after = command.execute()?;
        let changed = before.original() != after.document.original();
        apply(after)?;
        if changed {
            self.done.push(command);
            self.undone.clear();
        }
        Ok(())
    }
    pub fn undo(
        &mut self,
        apply: impl FnOnce(&EditState) -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(command) = self.done.last() {
            apply(command.undo())?;
            self.undone.push(self.done.pop().unwrap());
        }
        Ok(())
    }
    pub fn redo(
        &mut self,
        apply: impl FnOnce(&EditState) -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(command) = self.undone.last_mut() {
            apply(command.execute()?)?;
            self.done.push(self.undone.pop().unwrap());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> EditState {
        EditState {
            document: Arc::new(Document::parse("v {xschem file_version=1.3}\r\nC {cell.sym} 0 0 0 0 {name=X1}\r\n[\r\nL 4 -10 0 10 0 {}\r\n]\r\nN0 0 100 0 {lab=signal}\r\n").unwrap()),
            selection: BTreeSet::from([1, 3]),
        }
    }
    fn identity() -> Placement {
        Placement {
            pivot: Point::default(),
            at: Point::default(),
            rotation: 0,
            flip: false,
        }
    }
    fn execute(history: &mut CommandHistory, state: &mut EditState, operation: EditOperation) {
        let command = DocumentCommand::new(operation, state.clone());
        history
            .execute(Box::new(command), |next| {
                *state = next.clone();
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn moving_and_deleting_branches_rechecks_both_old_and_new_junctions() {
        let mut state = EditState {
            document: Arc::new(Document::parse("N 0 0 40 0 {}\nN 40 0 80 0 {}\nN 40 0 40 30 {}\nN 0 100 80 100 {}\n").unwrap()),
            selection: BTreeSet::from([2]),
        };
        let original = state.clone();
        let mut history = CommandHistory::default();
        execute(&mut history, &mut state, EditOperation::Move(Placement {
            at: Point::new(0., 100.), ..identity()
        }));
        assert_eq!(state.document.original(), "N 0 0 80 0 {}\nN 40 100 40 130 {}\nN 0 100 40 100 {}\nN 40 100 80 100 {}\n");
        assert_eq!(state.selection, BTreeSet::from([1]));
        let moved = state.clone();
        history.undo(|next| { state = next.clone(); Ok(()) }).unwrap();
        assert_eq!(state.document.original(), original.document.original());
        assert_eq!(state.selection, original.selection);
        history.redo(|next| { state = next.clone(); Ok(()) }).unwrap();
        assert_eq!(state.document.original(), moved.document.original());
        assert_eq!(state.selection, moved.selection);
        execute(&mut history, &mut state, EditOperation::Delete);
        assert_eq!(state.document.original(), "N 0 0 80 0 {}\nN 0 100 80 100 {}\n");
    }

    #[test]
    fn commands_restore_exact_documents_selections_and_copy_names() {
        let mut state = state();
        let original = state.clone();
        let mut history = CommandHistory::default();
        execute(
            &mut history,
            &mut state,
            EditOperation::Move(Placement {
                pivot: Point::new(5., 7.),
                at: Point::new(45., 67.),
                rotation: 1,
                flip: true,
            }),
        );
        let moved = state.clone();
        execute(&mut history, &mut state, EditOperation::Copy(identity()));
        let copied = state.clone();
        assert_eq!(state.selection, BTreeSet::from([3, 4])); // duplicate wire merges; copied instance shifts
        assert!(state.document.original().contains("name=\"X2\""));
        execute(&mut history, &mut state, EditOperation::Delete);
        assert!(state.selection.is_empty());
        assert_eq!(history.undo_label(), Some("Delete"));
        for expected in [&copied, &moved, &original] {
            history
                .undo(|next| {
                    state = next.clone();
                    Ok(())
                })
                .unwrap();
            assert_eq!(state.document.original(), expected.document.original());
            assert_eq!(state.selection, expected.selection);
        }
        assert!(history.undo_label().is_none());
        for expected in [&moved, &copied] {
            history
                .redo(|next| {
                    state = next.clone();
                    Ok(())
                })
                .unwrap();
            assert_eq!(state.document.original(), expected.document.original());
            assert_eq!(state.selection, expected.selection);
        }
        assert_eq!(history.redo_label(), Some("Delete"));
    }

    #[test]
    fn noops_and_failed_commands_preserve_redo_and_failed_restores_preserve_history() {
        let mut state = state();
        let mut history = CommandHistory::default();
        execute(&mut history, &mut state, EditOperation::Delete);
        assert!(history.undo(|_| Err("renderer failed".into())).is_err());
        assert_eq!(history.undo_label(), Some("Delete"));
        assert!(history.redo_label().is_none());
        history
            .undo(|next| {
                state = next.clone();
                Ok(())
            })
            .unwrap();
        let original = state.document.original().to_owned();
        let command = DocumentCommand::new(EditOperation::Copy(identity()), state.clone());
        assert!(
            history
                .execute(Box::new(command), |_| Err("renderer failed".into()))
                .is_err()
        );
        let mut invalid = state.clone();
        invalid.selection = BTreeSet::from([usize::MAX]);
        assert!(
            history
                .execute(
                    Box::new(DocumentCommand::new(EditOperation::Delete, invalid)),
                    |_| panic!("invalid command reached receiver")
                )
                .is_err()
        );
        assert!(history.redo(|_| Err("renderer failed".into())).is_err());
        execute(&mut history, &mut state, EditOperation::Move(identity()));
        execute(
            &mut history,
            &mut state,
            EditOperation::Wire {
                start: Point::default(),
                end: Point::default(),
            },
        );
        assert_eq!(state.document.original(), original);
        assert!(history.undo_label().is_none());
        assert_eq!(history.redo_label(), Some("Delete"));
        execute(&mut history, &mut state, EditOperation::Copy(identity()));
        assert_eq!(history.undo_label(), Some("Copy"));
        assert!(history.redo_label().is_none());
    }
}
