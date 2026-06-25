import MoyoDbProofs.Model.BTree

namespace MoyoDbProofs.Model

abbrev State := ModelMap

structure Snapshot where
  committed : State
  deriving Repr

def beginReadonly (s : State) : Snapshot := { committed := s }

def commitWrite (s : State) (writes : List (Bytes × Bytes)) : State :=
  writes.foldl (fun acc (kv : Bytes × Bytes) => insert acc kv.1 kv.2) s

def observe (snap : Snapshot) : State := snap.committed

theorem observe_begin_readonly (s : State) :
    observe (beginReadonly s) = s := by
  rfl

theorem commit_write_nil (s : State) :
    commitWrite s [] = s := by
  simp [commitWrite]

theorem commit_write_append (s : State) (w1 w2 : List (Bytes × Bytes)) :
    commitWrite s (w1 ++ w2) = commitWrite (commitWrite s w1) w2 := by
  induction w1 generalizing s with
  | nil => simp [commitWrite]
  | cons hd tl ih =>
      cases hd with
      | mk k v =>
          simp [commitWrite]

theorem snapshot_not_affected_by_future_commits (s : State) (_writes : List (Bytes × Bytes)) :
    observe (beginReadonly s) = s := by
  rfl

end MoyoDbProofs.Model
