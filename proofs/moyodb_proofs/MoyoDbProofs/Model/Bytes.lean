namespace MoyoDbProofs.Model

abbrev Bytes := List UInt8

def compareBytes : Bytes → Bytes → Ordering
  | [], [] => .eq
  | [], _ => .lt
  | _, [] => .gt
  | a :: as, b :: bs =>
      if a < b then .lt
      else if b < a then .gt
      else compareBytes as bs

def bytesLt (a b : Bytes) : Prop := compareBytes a b = .lt
def bytesLe (a b : Bytes) : Prop := let c := compareBytes a b; c = .lt ∨ c = .eq

instance : LT Bytes where
  lt := bytesLt

def inRange (k : Bytes) (gt gte lt lte : Option Bytes) : Bool :=
  let gtOk := match gt with | none => true | some b => compareBytes k b = .gt
  let gteOk :=
    match gte with
    | none => true
    | some b =>
        match compareBytes k b with
        | .gt => true
        | .eq => true
        | .lt => false
  let ltOk := match lt with | none => true | some b => compareBytes k b = .lt
  let lteOk :=
    match lte with
    | none => true
    | some b =>
        match compareBytes k b with
        | .lt => true
        | .eq => true
        | .gt => false
  gtOk && gteOk && ltOk && lteOk

theorem compare_refl (b : Bytes) : compareBytes b b = .eq := by
  induction b with
  | nil => simp [compareBytes]
  | cons x xs ih =>
      simp [compareBytes, ih]

end MoyoDbProofs.Model
