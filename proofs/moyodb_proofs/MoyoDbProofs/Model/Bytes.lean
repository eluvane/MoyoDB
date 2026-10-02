namespace MoyoDbProofs.Model

/-- Byte strings used for keys and encoded values. -/
abbrev Bytes := List UInt8

/-- Lexicographic comparison of unsigned bytes, with shorter prefixes first. -/
def compareBytes : Bytes → Bytes → Ordering
  | [], [] => .eq
  | [], _ => .lt
  | _, [] => .gt
  | a :: as, b :: bs =>
      if a < b then .lt
      else if b < a then .gt
      else compareBytes as bs

/-- Strict lexicographic ordering of byte strings. -/
def bytesLt (a b : Bytes) : Prop := compareBytes a b = .lt
/-- Non-strict lexicographic ordering of byte strings. -/
def bytesLe (a b : Bytes) : Prop := let c := compareBytes a b; c = .lt ∨ c = .eq

instance : LT Bytes where
  lt := bytesLt

/-- Check the four optional exclusive and inclusive scan bounds. -/
def inRange (k : Bytes) (gt gte lt lte : Option Bytes) : Bool :=
  let gtOk := match gt with | none => true | some b => compareBytes k b = .gt
  let gteOk :=
    match gte with
    | none => true
    | some b =>
        match compareBytes k b with
        | .gt | .eq => true
        | .lt => false
  let ltOk := match lt with | none => true | some b => compareBytes k b = .lt
  let lteOk :=
    match lte with
    | none => true
    | some b =>
        match compareBytes k b with
        | .lt | .eq => true
        | .gt => false
  gtOk && gteOk && ltOk && lteOk

/-- A byte string compares equal to itself. -/
theorem compare_refl (b : Bytes) : compareBytes b b = .eq := by
  induction b with
  | nil => simp [compareBytes]
  | cons x xs ih =>
      simp [compareBytes, ih]

end MoyoDbProofs.Model
