import MoyoDbProofs.Model.BTree

namespace MoyoDbProofs.Proofs
open MoyoDbProofs.Model

/-- The reference insertion exposes the inserted value to lookup. -/
theorem insert_lookup (m : ModelMap) (k v : Bytes) :
    lookup (insert m k v) k = some v :=
  lookup_insert_eq m k v

/-- The reference deletion makes the deleted key absent. -/
theorem delete_lookup_none (m : ModelMap) (k : Bytes) :
    lookup (erase m k) k = none :=
  lookup_erase_none m k

/-- Repeating a reference deletion does not change its result. -/
theorem delete_idempotent (m : ModelMap) (k : Bytes) :
    erase (erase m k) k = erase m k :=
  erase_idempotent m k

/-- Every returned scan row satisfies the requested bounds. -/
theorem scan_preserves_range_membership (m : ModelMap) (gt gte lt lte : Option Bytes) :
    ∀ kv ∈ scan m gt gte lt lte, inRange kv.1 gt gte lt lte = true := by
  intro kv hk
  simp [scan] at hk
  exact hk.2

/-- An unbounded reference scan returns every row in order. -/
theorem scan_without_bounds_is_identity (m : ModelMap) :
    scan m none none none none = m :=
  scan_none_bounds_identity m

end MoyoDbProofs.Proofs
