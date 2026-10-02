import MoyoDbProofs.Model.BTree

namespace MoyoDbProofs.Pages

open Model

/-- An upsert or deletion: a key paired with a present or absent encoded value. -/
abbrev Mutation := Bytes × Option Bytes

/-- Apply mutations sequentially to the independent reference map. -/
def applyBatch (state : ModelMap) (mutations : List Mutation) : ModelMap :=
  mutations.foldl (fun rows mutation =>
    match mutation.2 with
    | none => erase rows mutation.1
    | some value => insert rows mutation.1 value) state

/-- Concatenated mutation batches have the same effect as sequential batches. -/
theorem applyBatch_append (state : ModelMap) (first second : List Mutation) :
    applyBatch state (first ++ second) = applyBatch (applyBatch state first) second := by
  simp [applyBatch, List.foldl_append]

/-- A decoded physical tree. Sharing across roots is allowed, within a root is checked. -/
inductive Tree where
  /-- A root-zero empty tree with no physical page. -/
  | empty
  /-- A physical leaf and its decoded rows. -/
  | leaf (pageId : Nat) (rows : ModelMap)
  /-- An internal page with its level and ordered minimum-key child separators. -/
  | branch (pageId level : Nat) (children : List (Bytes × Tree))
  deriving Repr

/-- The stored internal height; empty trees and leaves have height zero. -/
def level : Tree → Nat
  | .empty => 0
  | .leaf _ _ => 0
  | .branch _ n _ => n

/-- Collect leaf rows in child order, bounded by the supplied traversal fuel. -/
def flatten : Nat → Tree → ModelMap
  | 0, _ => []
  | _ + 1, .empty => []
  | _ + 1, .leaf _ rows => rows
  | n + 1, .branch _ _ children => children.flatMap (flatten n ·.2)

/-- Collect reachable physical page identifiers, bounded by traversal fuel. -/
def pageIds : Nat → Tree → List Nat
  | 0, _ => []
  | _ + 1, .empty => []
  | _ + 1, .leaf id _ => [id]
  | n + 1, .branch id _ children =>
      id :: children.flatMap (pageIds n ·.2)

/-- Last separator at or below the key, falling back to the first child. -/
def chooseChild (children : List (Bytes × Tree)) (key : Bytes) : Option (Bytes × Tree) :=
  match (children.filter (compareBytes ·.1 key != .gt)).getLast? with
  | some child => some child
  | none => children.head?

/-- Follow internal separators and look up the key in the selected leaf. -/
def route : Nat → Tree → Bytes → Option Bytes
  | 0, _, _ => none
  | _ + 1, .empty, _ => none
  | _ + 1, .leaf _ rows, key => lookup rows key
  | n + 1, .branch _ _ children, key =>
      match chooseChild children key with
      | none => none
      | some child => route n child.2 key

/-- Decide whether every earlier row has a strictly smaller key than every later row. -/
def strictlySorted (rows : ModelMap) : Bool :=
  decide (rows.Pairwise (fun a b => compareBytes a.1 b.1 = .lt))

/-- Check page identifiers, levels, minimum separators, sorted rows and unique reachability. -/
def structureOK : Nat → Tree → Bool
  | 0, _ => false
  | _ + 1, .empty => true
  | _ + 1, .leaf id rows => id != 0 && strictlySorted rows
  | n + 1, tree@(.branch id height children) =>
      id != 0 && height != 0 && height <= 48 && !children.isEmpty &&
      children.all (fun child =>
        structureOK n child.2 &&
        decide (level child.2 + 1 = height) &&
        decide ((flatten n child.2).head?.map Prod.fst = some child.1)) &&
      strictlySorted (flatten (n + 1) tree) &&
      decide ((pageIds (n + 1) tree).Nodup)

/-- Checking every present key, plus route soundness below, covers absent keys too. -/
def routingOK (fuel : Nat) (tree : Tree) : Bool :=
  (flatten fuel tree).all (fun kv => decide (route fuel tree kv.1 = some kv.2))

/-- A selected child always belongs to the supplied child list. -/
theorem chooseChild_mem {children : List (Bytes × Tree)} {key : Bytes}
    {child : Bytes × Tree} (h : chooseChild children key = some child) :
    child ∈ children := by
  unfold chooseChild at h
  cases he : (children.filter (compareBytes ·.1 key != .gt)).getLast? with
  | none =>
      simp only [he] at h
      exact List.mem_of_head? h
  | some selected =>
      simp only [he, Option.some.injEq] at h
      subst selected
      exact (List.mem_filter.mp (List.mem_of_getLast? he)).1

/-- A successful reference lookup corresponds to a row in the map. -/
theorem lookup_some_mem {rows : ModelMap} {key value : Bytes}
    (h : lookup rows key = some value) : (key, value) ∈ rows := by
  induction rows with
  | nil => simp [lookup] at h
  | cons kv tail ih =>
      rcases kv with ⟨k, v⟩
      by_cases hk : k = key
      · subst k
        simp [lookup] at h
        subst v
        exact List.mem_cons_self
      · have ht : lookup tail key = some value := by simpa [lookup, hk] using h
        exact List.mem_cons_of_mem _ (ih ht)

/-- A missing reference key cannot occur in any row. -/
theorem lookup_none_not_mem {rows : ModelMap} {key value : Bytes}
    (h : lookup rows key = none) : (key, value) ∉ rows := by
  induction rows with
  | nil => simp
  | cons kv tail ih =>
      rcases kv with ⟨k, v⟩
      by_cases hk : k = key
      · simp [lookup, hk] at h
      · have ht : lookup tail key = none := by simpa [lookup, hk] using h
        intro hm
        rcases List.mem_cons.mp hm with hm | hm
        · exact hk (congrArg Prod.fst hm.symm)
        · exact ih ht hm

/-- Routing cannot invent an entry, independently of ordering or valid separators. -/
theorem route_some_mem (fuel : Nat) (tree : Tree) (key value : Bytes)
    (h : route fuel tree key = some value) : (key, value) ∈ flatten fuel tree := by
  induction fuel generalizing tree with
  | zero => simp [route] at h
  | succ n ih =>
      cases tree with
      | empty => simp [route] at h
      | leaf id rows => exact lookup_some_mem h
      | branch id height children =>
          cases hc : chooseChild children key with
          | none => simp [route, hc] at h
          | some child =>
              have ht : route n child.2 key = some value := by
                simpa [route, hc] using h
              exact List.mem_flatMap.mpr ⟨child, chooseChild_mem hc, ih child.2 ht⟩

/-- A finite check of present keys certifies routing for EVERY byte-string key. -/
theorem routing_refines_lookup (fuel : Nat) (tree : Tree)
    (h : routingOK fuel tree = true) (key : Bytes) :
    route fuel tree key = lookup (flatten fuel tree) key := by
  have hall : ∀ kv ∈ flatten fuel tree, route fuel tree kv.1 = some kv.2 := by
    simpa [routingOK] using (List.all_eq_true.mp h)
  cases hl : lookup (flatten fuel tree) key with
  | none =>
      cases hr : route fuel tree key with
      | none => exact rfl
      | some value =>
          exact False.elim (lookup_none_not_mem hl (route_some_mem fuel tree key value hr))
  | some value => exact hall (key, value) (lookup_some_mem hl)

/-- Traversal fuel allowing at most 48 internal levels followed by one leaf. -/
def depthBudget : Nat := 49

/-- Proofs erase at runtime; accepted data carries the checked obligations. -/
structure CertifiedTree (expected : ModelMap) where
  /-- The exact decoded tree accepted by certification. -/
  tree : Tree
  /-- Structural checks succeeded for the complete depth budget. -/
  wellFormed : structureOK depthBudget tree = true
  /-- Routing finds the correct value for every present key. -/
  routing : routingOK depthBudget tree = true
  /-- Flattened rows coincide with the independently computed reference map. -/
  contents : flatten depthBudget tree = expected

/-- Return a certificate only after structural, routing and reference-content checks succeed. -/
def certify (tree : Tree) (expected : ModelMap) : Option (CertifiedTree expected) :=
  if hs : structureOK depthBudget tree = true then
    if hr : routingOK depthBudget tree = true then
      if he : flatten depthBudget tree = expected then some ⟨tree, hs, hr, he⟩
      else none
    else none
  else none

/-- Certification preserves the exact tree supplied to the checker. -/
theorem certify_tree {tree : Tree} {expected : ModelMap} {checked : CertifiedTree expected}
    (h : certify tree expected = some checked) : checked.tree = tree := by
  unfold certify at h
  split at h
  · split at h
    · split at h
      · exact congrArg CertifiedTree.tree (Option.some.inj h).symm
      · contradiction
    · contradiction
  · contradiction

/-- A certified tree agrees with the reference map for every byte-string key. -/
theorem certified_lookup (expected : ModelMap) (checked : CertifiedTree expected) (key : Bytes) :
    route depthBudget checked.tree key = lookup expected key := by
  exact (routing_refines_lookup depthBudget checked.tree checked.routing key).trans
    (congrArg (lookup · key) checked.contents)

/-- The expected map of a certified tree has strictly increasing keys. -/
theorem certified_sorted (expected : ModelMap) (checked : CertifiedTree expected) :
    strictlySorted expected = true := by
  have hs : strictlySorted (flatten depthBudget checked.tree) = true := by
    have h := checked.wellFormed
    cases ht : checked.tree with
    | empty => simp [depthBudget, flatten, strictlySorted]
    | leaf id rows =>
        simp [depthBudget, structureOK, ht] at h
        simpa [depthBudget, ht, flatten] using h.2
    | branch id height children =>
        simp only [ht, depthBudget, structureOK, Bool.and_eq_true] at h
        exact h.1.2
  exact (congrArg strictlySorted checked.contents).symm.trans hs

end MoyoDbProofs.Pages
