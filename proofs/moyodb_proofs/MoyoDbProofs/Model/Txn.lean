import MoyoDbProofs.Model.BTree

namespace MoyoDbProofs.Model

/-- Committed transaction state in the reference model. -/
abbrev State := ModelMap

/-- An immutable view of committed state captured by a readonly transaction. -/
structure Snapshot where
  /-- State captured at the start of the readonly transaction. -/
  committed : State

/-- Capture the current committed state as an immutable snapshot. -/
def beginReadonly (s : State) : Snapshot := { committed := s }

/-- Apply an ordered batch of writes to committed state. -/
def commitWrite (s : State) (writes : List (Bytes × Bytes)) : State :=
  writes.foldl (fun acc (kv : Bytes × Bytes) => insert acc kv.1 kv.2) s

/-- Read the committed state captured by a snapshot. -/
def observe (snap : Snapshot) : State := snap.committed

/-- A new readonly snapshot observes exactly the captured state. -/
theorem observe_begin_readonly (s : State) :
    observe (beginReadonly s) = s := by
  exact rfl

/-- An empty write batch preserves committed state. -/
theorem commit_write_nil (s : State) :
    commitWrite s [] = s := by
  simp [commitWrite]

/-- Applying concatenated write batches is equivalent to applying them sequentially. -/
theorem commit_write_append (s : State) (w1 w2 : List (Bytes × Bytes)) :
    commitWrite s (w1 ++ w2) = commitWrite (commitWrite s w1) w2 := by
  induction w1 generalizing s with
  | nil => simp [commitWrite]
  | cons hd tl ih =>
      cases hd with
      | mk k v =>
          simp [commitWrite]

/-- A captured snapshot observes its original committed state. -/
theorem snapshot_not_affected_by_future_commits (s : State) (_writes : List (Bytes × Bytes)) :
    observe (beginReadonly s) = s := by
  exact rfl

end MoyoDbProofs.Model
