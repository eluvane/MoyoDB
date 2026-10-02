import MoyoDbProofs.Model.BTree

namespace MoyoDbProofs.Model

/-- Abstract WAL records retaining transaction and page identifiers. -/
inductive WalRecord where
  /-- A page image written by a transaction, without implying commitment. -/
  | pageImage (txid pageId : Nat)
  /-- The commit marker making a transaction recoverable. -/
  | commit (txid : Nat)
  deriving Repr, DecidableEq

/-- Extract the transaction identifier from either WAL record kind. -/
def recordTxid : WalRecord → Nat
  | .pageImage txid _ => txid
  | .commit txid => txid

/-- Collect commit markers in log order; page images contribute no commits. -/
def committedTxs : List WalRecord → List Nat
  | [] => []
  | .pageImage _ _ :: xs => committedTxs xs
  | .commit txid :: xs => txid :: committedTxs xs

/-- The abstract committed transaction sequence recovered from a log. -/
def replayCommittedPrefix : List WalRecord → List Nat :=
  committedTxs

/-- Collecting commits distributes over concatenated log segments. -/
theorem committed_txs_append (xs ys : List WalRecord) :
    committedTxs (xs ++ ys) = committedTxs xs ++ committedTxs ys := by
  induction xs with
  | nil => simp [committedTxs]
  | cons x xs ih =>
      cases x <;> simp [committedTxs, ih]

/-- A trailing page image without a commit marker does not change recovered commits. -/
theorem incomplete_tail_ignored (xs : List WalRecord) (txid pageId : Nat) :
    replayCommittedPrefix (xs ++ [WalRecord.pageImage txid pageId]) = replayCommittedPrefix xs := by
  simp [replayCommittedPrefix, committed_txs_append, committedTxs]

/-- A trailing commit marker appends its transaction to recovered commits. -/
theorem append_clean_commit (xs : List WalRecord) (txid : Nat) :
    replayCommittedPrefix (xs ++ [WalRecord.commit txid]) = replayCommittedPrefix xs ++ [txid] := by
  simp [replayCommittedPrefix, committed_txs_append, committedTxs]

end MoyoDbProofs.Model
