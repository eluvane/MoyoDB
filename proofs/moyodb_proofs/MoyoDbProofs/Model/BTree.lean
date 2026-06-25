import MoyoDbProofs.Model.Bytes

namespace MoyoDbProofs.Model

abbrev KV := Bytes × Bytes
abbrev ModelMap := List KV

def keys : ModelMap → List Bytes
  | [] => []
  | (k, _) :: xs => k :: keys xs

def lookup (m : ModelMap) (key : Bytes) : Option Bytes :=
  match m with
  | [] => none
  | (k, v) :: xs =>
      if k = key then some v else lookup xs key

def erase (m : ModelMap) (key : Bytes) : ModelMap :=
  match m with
  | [] => []
  | (k, v) :: xs =>
      if k = key then erase xs key else (k, v) :: erase xs key

def insertSorted (key value : Bytes) : ModelMap → ModelMap
  | [] => [(key, value)]
  | (k, v) :: xs =>
      match compareBytes key k with
      | .lt => (key, value) :: (k, v) :: xs
      | .eq => (key, value) :: xs
      | .gt => (k, v) :: insertSorted key value xs

def insert (m : ModelMap) (key value : Bytes) : ModelMap :=
  let without := erase m key
  insertSorted key value without

def scan (m : ModelMap) (gt gte lt lte : Option Bytes) : ModelMap :=
  m.filter (fun (kv : KV) => inRange kv.1 gt gte lt lte)

def sortedUnique : ModelMap → Prop
  | [] => True
  | [_] => True
  | (k1, _) :: (k2, _) :: xs =>
      compareBytes k1 k2 = .lt ∧ sortedUnique ((k2, []) :: xs)

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

theorem lookup_insert_eq (m : ModelMap) (k v : Bytes) :
    lookup (insert m k v) k = some v := by
  unfold insert
  exact lookup_insert_sorted_eq (erase m k) k v

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

theorem scan_none_bounds_identity (m : ModelMap) :
    scan m none none none none = m := by
  induction m with
  | nil => simp [scan, inRange]
  | cons hd tl ih =>
      cases hd with
      | mk k v =>
          simp [scan, inRange]

end MoyoDbProofs.Model
