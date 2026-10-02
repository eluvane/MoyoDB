import MoyoDbProofs.Model.Bytes

namespace MoyoDbProofs.Model

/-- A key and its encoded value. -/
abbrev KV := Bytes × Bytes
/-- The reference map, represented by ordered key-value rows. -/
abbrev ModelMap := List KV

/-- Extract keys in the existing row order. -/
def keys : ModelMap → List Bytes
  | [] => []
  | (k, _) :: xs => k :: keys xs

/-- Return the value of the first row matching the key, or no value. -/
def lookup (m : ModelMap) (key : Bytes) : Option Bytes :=
  match m with
  | [] => none
  | (k, v) :: xs =>
      if k = key then some v else lookup xs key

/-- Remove every row matching the key. -/
def erase (m : ModelMap) (key : Bytes) : ModelMap :=
  match m with
  | [] => []
  | (k, v) :: xs =>
      if k = key then erase xs key else (k, v) :: erase xs key

/-- Insert or replace a row while preserving lexicographic order. -/
def insertSorted (key value : Bytes) : ModelMap → ModelMap
  | [] => [(key, value)]
  | (k, v) :: xs =>
      match compareBytes key k with
      | .lt => (key, value) :: (k, v) :: xs
      | .eq => (key, value) :: xs
      | .gt => (k, v) :: insertSorted key value xs

/-- Remove previous occurrences, then insert the replacement in order. -/
def insert (m : ModelMap) (key value : Bytes) : ModelMap :=
  let without := erase m key
  insertSorted key value without

/-- Keep rows satisfying all supplied scan bounds, preserving order. -/
def scan (m : ModelMap) (gt gte lt lte : Option Bytes) : ModelMap :=
  m.filter (fun (kv : KV) => inRange kv.1 gt gte lt lte)

/-- Consecutive keys are strictly increasing and therefore unique. -/
def sortedUnique : ModelMap → Prop
  | [] => True
  | [_] => True
  | (k1, _) :: (k2, _) :: xs =>
      compareBytes k1 k2 = .lt ∧ sortedUnique ((k2, []) :: xs)

/-- Looking up a freshly inserted key returns the inserted value. -/
theorem lookup_insert_sorted_eq (m : ModelMap) (k v : Bytes) :
    lookup (insertSorted k v m) k = some v := by
  induction m with
  | nil =>
      simp [insertSorted, lookup]
  | cons hd tl ih =>
      cases hd with
      | mk hk hv =>
          cases hCmp : compareBytes k hk with
          | lt =>
              simp [insertSorted, hCmp, lookup]
          | eq =>
              simp [insertSorted, hCmp, lookup]
          | gt =>
              by_cases hEq : hk = k
              · subst hk
                simp [compare_refl] at hCmp
              · simp [insertSorted, hCmp, lookup, hEq, ih]

/-- Replacement through the reference map returns the new value. -/
theorem lookup_insert_eq (m : ModelMap) (k v : Bytes) :
    lookup (insert m k v) k = some v := by
  unfold insert
  exact lookup_insert_sorted_eq (erase m k) k v

/-- An erased key cannot be found in the resulting map. -/
theorem lookup_erase_none (m : ModelMap) (k : Bytes) :
    lookup (erase m k) k = none := by
  induction m with
  | nil => simp [erase, lookup]
  | cons hd tl ih =>
      cases hd with
      | mk hk hv =>
          by_cases hEq : hk = k
          · simp [erase, hEq, ih]
          · simp [erase, lookup, hEq, ih]

/-- Erasing the same key twice has the same effect as erasing it once. -/
theorem erase_idempotent (m : ModelMap) (k : Bytes) :
    erase (erase m k) k = erase m k := by
  induction m with
  | nil => simp [erase]
  | cons hd tl ih =>
      cases hd with
      | mk hk hv =>
          by_cases hEq : hk = k
          · simp [erase, hEq, ih]
          · simp [erase, hEq, ih]

/-- A scan with no bounds returns the entire map. -/
theorem scan_none_bounds_identity (m : ModelMap) :
    scan m none none none none = m := by
  induction m with
  | nil => simp [scan, inRange]
  | cons hd tl ih =>
      cases hd with
      | mk k v =>
          simp [scan, inRange]

end MoyoDbProofs.Model
