import MoyoDbProofs.Model.Txn

namespace MoyoDbProofs.Proofs
open MoyoDbProofs.Model

theorem readonlySnapshotStable (s : State) (writes : List (Bytes × Bytes)) :
    observe (beginReadonly s) = s :=
  snapshot_not_affected_by_future_commits s writes

theorem emptyWriteBatchIsIdentity (s : State) :
    commitWrite s [] = s :=
  commit_write_nil s

theorem sequentialWritesSerializable (s : State) (w1 w2 : List (Bytes × Bytes)) :
    commitWrite s (w1 ++ w2) = commitWrite (commitWrite s w1) w2 :=
  commit_write_append s w1 w2

end MoyoDbProofs.Proofs
