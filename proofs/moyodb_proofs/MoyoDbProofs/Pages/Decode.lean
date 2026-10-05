import MoyoDbProofs.Pages.Core

namespace MoyoDbProofs.Pages

open Model

/-- A physical page identifier paired with its complete raw image. -/
structure RawPage where
  /-- Page identifier supplied by the storage backend. -/
  id : Nat
  /-- Complete page image, interpreted independently from its header. -/
  bytes : Bytes

/-- Convert absence to an explicit diagnostic instead of using a default value. -/
def expectSome {α : Type} (value : Option α) (message : String) : Except String α :=
  match value with
  | some result => .ok result
  | none => .error message

/-- Read a byte range only when the entire requested range is present. -/
def checkedSlice (bytes : Bytes) (offset count : Nat) : Option Bytes :=
  if offset + count <= bytes.length then some ((bytes.drop offset).take count) else none

/-- Successful slicing establishes that the requested end lies within the input. -/
theorem checkedSlice_bounds {bytes slice : Bytes} {offset count : Nat}
    (h : checkedSlice bytes offset count = some slice) :
    offset + count <= bytes.length := by
  unfold checkedSlice at h
  split at h
  · assumption
  · contradiction

/-- A successful slice has exactly the requested byte count. -/
theorem checkedSlice_length {bytes slice : Bytes} {offset count : Nat}
    (h : checkedSlice bytes offset count = some slice) : slice.length = count := by
  have hb := checkedSlice_bounds h
  unfold checkedSlice at h
  split at h
  · have he := Option.some.inj h
    rw [← he, List.length_take, List.length_drop, Nat.min_eq_left]
    exact Nat.le_sub_of_add_le' hb
  · contradiction

/-- Read a checked byte range or report its offset and length. -/
def readBytes (bytes : Bytes) (offset count : Nat) : Except String Bytes :=
  expectSome (checkedSlice bytes offset count) s!"byte range {offset}+{count} outside page"

/-- Decode a checked little-endian integer without relying on Rust-decoded fields. -/
def readLE (bytes : Bytes) (offset count : Nat) : Except String Nat := do
  let data ← readBytes bytes offset count
  return (data.foldr (fun byte acc => byte.toNat + 256 * acc) 0)

/-- An independently decoded leaf or internal cell, including its occupied byte range. -/
structure Cell where
  /-- First occupied byte in the page image. -/
  start : Nat
  /-- Exclusive end of the occupied byte range. -/
  finish : Nat
  /-- Leaf key or internal minimum-key separator. -/
  key : Bytes
  /-- Inline leaf value; internal cells leave it empty. -/
  value : Bytes := []
  /-- Internal child identifier; leaf cells leave it zero. -/
  child : Nat := 0

/-- The header and cells decoded from one raw physical page. -/
structure Page where
  /-- Nonzero identifier decoded from the header. -/
  id : Nat
  /-- Wire kind: one for leaves, two for internal pages. -/
  kind : Nat
  /-- Internal height decoded from the header, zero for leaves. -/
  height : Nat
  /-- Independently decoded cells in slot-table order. -/
  cells : List Cell

/-- Reject a failed runtime condition with a deterministic diagnostic. -/
def require (condition : Bool) (message : String) : Except String Unit :=
  if condition then .ok () else .error message

/-- Parse raw page bytes without Rust-decoded fields. Checksums are not assumed or validated. -/
def decodePage (raw : RawPage) : Except String Page := do
  let bytes := raw.bytes
  require (bytes.length == 4096) "page size must be 4096"
  require (bytes.take 4 == [80, 65, 71, 49]) "page magic"
  let id ← readLE bytes 8 8
  require (id == raw.id && id != 0) "page id mismatch"
  let kind ← readLE bytes 16 1
  require (kind == 1 || kind == 2) "unsupported page kind (overflow is outside this checker)"
  let height ← readLE bytes 17 1
  let count ← readLE bytes 18 2
  let lower ← readLE bytes 20 2
  let upper ← readLE bytes 22 2
  require (decide (34 + 2 * count <= lower ∧ lower <= upper ∧ upper <= 4096))
    "header/slot-table bounds"
  require (if kind == 1 then height == 0 else height > 0 && height <= 48 && count > 0)
    "kind/level/child-count"
  let mut cells : List Cell := []
  for index in List.range count do
    let start ← readLE bytes (34 + 2 * index) 2
    require (decide (upper <= start ∧ start < 4096)) "cell slot outside payload"
    let keyLength ← readLE bytes start 2
    let cell ← if kind == 1 then do
      let valueKind ← readLE bytes (start + 2) 1
      require (valueKind == 1) "unsupported overflow value"
      let totalLength ← readLE bytes (start + 4) 4
      let overflow ← readLE bytes (start + 8) 8
      let valueLength ← readLE bytes (start + 16) 4
      require (totalLength == valueLength && overflow == 0) "inline value metadata"
      let key ← readBytes bytes (start + 20) keyLength
      let value ← readBytes bytes (start + 20 + keyLength) valueLength
      pure { start, finish := start + 20 + keyLength + valueLength, key, value : Cell }
    else do
      let child ← readLE bytes (start + 4) 8
      require (child != 0) "internal child is zero"
      let key ← readBytes bytes (start + 12) keyLength
      pure { start, finish := start + 12 + keyLength, key, child : Cell }
    require (cells.all (fun previous =>
      decide (cell.finish <= previous.start ∨ previous.finish <= cell.start)))
      "overlapping cells"
    cells := cells ++ [cell]
  return { id, kind, height, cells }

/-- The visited set is per root: COW roots may legally share immutable pages. -/
def decodeTree : Nat → List RawPage → Nat → List Nat → Except String (Tree × List Nat)
  | 0, _, id, _ => .error s!"page {id}: depth budget exceeded"
  | fuel + 1, pages, id, visited => do
      require (id != 0) "zero child"
      require (!visited.contains id) s!"page {id}: cycle or repeated reachability"
      let raw ← expectSome (pages.find? (fun page => page.id == id)) s!"page {id}: missing reachable page"
      let page ← (decodePage raw).mapError (s!"page {id}: {·}")
      let mut seen := id :: visited
      if page.kind == 1 then
        return (.leaf id (page.cells.map (fun cell => (cell.key, cell.value))), seen)
      else
        let mut children : List (Bytes × Tree) := []
        for cell in page.cells do
          let (child, nextSeen) ← decodeTree fuel pages cell.child seen
          seen := nextSeen
          require (level child + 1 == page.height) s!"page {id}: child level mismatch"
          require ((flatten fuel child).head?.map Prod.fst == some cell.key)
            s!"page {id}: separator/minimum mismatch for child {cell.child}"
          children := children ++ [(cell.key, child)]
        return (.branch id page.height children, seen)

/-- Interpret root zero as empty; otherwise decode the reachable physical tree. -/
def interpretRawTree (pages : List RawPage) (root : Nat) : Except String Tree :=
  if root == 0 then .ok .empty else (decodeTree depthBudget pages root []).map Prod.fst

/-- Runtime acceptance returns the certificate used by the soundness theorems. -/
def checkRawTree (pages : List RawPage) (root : Nat) (expected : ModelMap) :
    Except String (CertifiedTree expected) :=
  if (pages.map RawPage.id).Nodup then
    match interpretRawTree pages root with
    | .error message => .error message
    | .ok tree => expectSome (certify tree expected) "tree structure/routing/content mismatch"
  else .error "duplicate raw page ids"

/-- The certified tree is the interpretation of these exact raw pages and root. -/
theorem raw_checker_source {pages : List RawPage} {root : Nat} {expected : ModelMap}
    {checked : CertifiedTree expected} (h : checkRawTree pages root expected = .ok checked) :
    interpretRawTree pages root = .ok checked.tree := by
  unfold checkRawTree at h
  split at h
  · cases hd : interpretRawTree pages root with
    | error message => simp [hd] at h
    | ok tree =>
        cases hc : certify tree expected with
        | none => simp [hd, hc, expectSome] at h
        | some candidate =>
            have he : candidate = checked := by simpa [hd, hc, expectSome] using h
            subst candidate
            simp [certify_tree hc]
  · contradiction

/-- Accepted raw pages yield a well-formed tree whose lookup refines the expected map. -/
theorem raw_checker_sound {pages : List RawPage} {root : Nat} {expected : ModelMap}
    {checked : CertifiedTree expected} (h : checkRawTree pages root expected = .ok checked) :
    interpretRawTree pages root = .ok checked.tree ∧
    structureOK depthBudget checked.tree = true ∧
    ∀ key, route depthBudget checked.tree key = lookup expected key :=
  ⟨raw_checker_source h, checked.wellFormed, certified_lookup expected checked⟩

end MoyoDbProofs.Pages
