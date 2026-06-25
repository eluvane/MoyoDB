import MoyoDbProofs.Model.WAL

namespace MoyoDbProofs.Proofs
open MoyoDbProofs.Model

theorem incompleteTailIgnored (xs : List WalRecord) (txid pageId : Nat) :
    replayCommittedPrefix (xs ++ [WalRecord.pageImage txid pageId]) = replayCommittedPrefix xs :=
  incomplete_tail_ignored xs txid pageId

theorem cleanCommitAppears (xs : List WalRecord) (txid : Nat) :
    txid ∈ replayCommittedPrefix (xs ++ [WalRecord.commit txid]) := by
  rw [append_clean_commit]
  simp

theorem cleanCommitAppendsToCommittedPrefix (xs : List WalRecord) (txid : Nat) :
    replayCommittedPrefix (xs ++ [WalRecord.commit txid]) = replayCommittedPrefix xs ++ [txid] :=
  append_clean_commit xs txid

end MoyoDbProofs.Proofs
