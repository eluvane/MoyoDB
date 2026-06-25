import MoyoDbProofs.Model.BTree

namespace MoyoDbProofs.Model

inductive WalRecord where
  | pageImage (txid : Nat) (pageId : Nat)
  | commit (txid : Nat)
  deriving Repr, DecidableEq

def recordTxid : WalRecord → Nat
  | .pageImage txid _ => txid
  | .commit txid => txid

def committedTxs : List WalRecord → List Nat
  | [] => []
  | .pageImage _ _ :: xs => committedTxs xs
  | .commit txid :: xs => txid :: committedTxs xs

def replayCommittedPrefix : List WalRecord → List Nat :=
  committedTxs

theorem committed_txs_append (xs ys : List WalRecord) :
    committedTxs (xs ++ ys) = committedTxs xs ++ committedTxs ys := by
  induction xs with
  | nil => simp [committedTxs]
  | cons x xs ih =>
      cases x <;> simp [committedTxs, ih]

theorem incomplete_tail_ignored (xs : List WalRecord) (txid pageId : Nat) :
    replayCommittedPrefix (xs ++ [WalRecord.pageImage txid pageId]) = replayCommittedPrefix xs := by
  simp [replayCommittedPrefix, committed_txs_append, committedTxs]

theorem append_clean_commit (xs : List WalRecord) (txid : Nat) :
    replayCommittedPrefix (xs ++ [WalRecord.commit txid]) = replayCommittedPrefix xs ++ [txid] := by
  simp [replayCommittedPrefix, committed_txs_append, committedTxs]

end MoyoDbProofs.Model
