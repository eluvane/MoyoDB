import Lean.Data.Json.Parser
import Lean.Data.Json.FromToJson
import MoyoDbProofs.Pages.Decode

open Lean MoyoDbProofs.Model MoyoDbProofs.Pages

namespace BTreePageConformance

/-- Decode a lowercase hexadecimal digit or reject invalid input. -/
def hexDigit (char : Char) : Except String Nat :=
  let code := char.toNat
  if _h : 48 <= code ∧ code <= 57 then .ok (code - 48)
  else if _h : 97 <= code ∧ code <= 102 then .ok (code - 97 + 10)
  else .error "hex must contain lowercase pairs"

/-- Decode hexadecimal byte pairs, rejecting invalid digits and odd lengths. -/
def decodeHex : List Char → Except String Bytes
  | [] => .ok []
  | first :: second :: rest => do
      let a ← hexDigit first
      let b ← hexDigit second
      let tail ← decodeHex rest
      return UInt8.ofNat (16 * a + b) :: tail
  | _ => .error "odd hex length"

/-- Read a JSON string as hexadecimal bytes. -/
def bytes (json : Json) : Except String Bytes := do
  decodeHex (← json.getStr?).toList

/-- Read JSON null as absence, otherwise decode hexadecimal bytes. -/
def optionalBytes (json : Json) : OptionT (Except String) Bytes :=
  OptionT.mk (if json == .null then .ok none else some <$> bytes json)

/-- Read a named JSON natural-number field. -/
def natField (json : Json) (name : String) : Except String Nat :=
  json.getObjValAs? Nat name

/-- Read a named JSON array field. -/
def arrayField (json : Json) (name : String) : Except String (Array Json) := do
  (← json.getObjVal? name).getArr?

/-- Read a named JSON hexadecimal-byte field. -/
def byteField (json : Json) (name : String) : Except String Bytes := do
  bytes (← json.getObjVal? name)

/-- Read a named nullable hexadecimal-byte field. -/
def optionalField (json : Json) (name : String) : OptionT (Except String) Bytes :=
  OptionT.mk do
    (optionalBytes (← json.getObjVal? name)).run

/-- Decode key-value rows, requiring exactly two fields per row. -/
def parseRows (json : Json) : Except String ModelMap := do
  (← json.getArr?).toList.mapM fun row => do
    let pair ← row.getArr?
    match pair.toList with
    | [key, value] => return (← bytes key, ← bytes value)
    | _ => throw "row must contain key and value"

/-- Decode bounded raw page images from a corpus frame. -/
def parsePages (frame : Json) : Except String (List RawPage) := do
  let pages ← arrayField frame "pages"
  require (pages.size <= 256) "page budget exceeded"
  pages.toList.mapM fun json => do
    let id ← natField json "id"
    let hex ← json.getObjValAs? String "bytes_hex"
    require (hex.length == 8192) "raw page must have exactly 8192 hex digits"
    return { id, bytes := ← decodeHex hex.toList }

/-- Decode a bounded mutation batch and require sorted, unique keys. -/
def parseMutations (frame : Json) : Except String (List (Bytes × Option Bytes)) := do
  let mutations ← arrayField frame "mutations"
  require (mutations.size <= 128) "mutation budget exceeded"
  let result ← mutations.toList.mapM fun mutation => do
    return (← byteField mutation "key_hex", ← (optionalField mutation "value_hex").run)
  require (strictlySorted (result.map (·.1, [])))
    "mutation keys must be sorted and unique"
  return result

/-- Compare observed lookups to the model and require coverage of known keys and sentinels. -/
def checkLookups (frame : Json) (expected : ModelMap) (known : List Bytes) : Except String Unit := do
  let lookups ← arrayField frame "lookups"
  require (lookups.size <= 256) "lookup budget exceeded"
  let mut queried : List Bytes := []
  for query in lookups do
    let key ← byteField query "key_hex"
    let actual ← (optionalField query "value_hex").run
    require (actual == lookup expected key) "lookup mismatch"
    queried := key :: queried
  require ((known ++ [[], [0], [255, 255, 255]]).all queried.contains)
    "missing mandatory lookup"

/-- Compare observed ordered scans to the model and require the mandatory scan modes. -/
def checkScans (frame : Json) (expected : ModelMap) : Except String Unit := do
  let scans ← arrayField frame "scans"
  require (scans.size >= 5 && scans.size <= 16) "missing scans or scan budget exceeded"
  let mut modes : List Nat := []
  for query in scans do
    let gt ← (optionalField query "gt").run
    let gte ← (optionalField query "gte").run
    let lt ← (optionalField query "lt").run
    let lte ← (optionalField query "lte").run
    let reverse ← query.getObjValAs? Bool "reverse"
    let rawLimit ← query.getObjVal? "limit"
    let limit ← if rawLimit == .null then pure none else some <$> fromJson? (α := Nat) rawLimit
    let filtered := scan expected gt gte lt lte
    let ordered := if reverse then filtered.reverse else filtered
    let wanted := match limit with | none => ordered | some n => ordered.take n
    let actual ← parseRows (← query.getObjVal? "rows")
    require (actual == wanted) "scan mismatch"
    if gt.isNone && gte.isNone && lt.isNone && lte.isNone then
      let mode := match reverse, limit with
        | false, none => 0
        | true, none => 1
        | false, some 0 => 2
        | false, some 1 => 3
        | true, some 3 => 4
        | _, _ => 5
      modes := mode :: modes
  require ([0, 1, 2, 3, 4].all modes.contains)
    "missing mandatory scan mode"

/-- Replay one complete scenario, checking current pages and all retained COW roots. -/
def checkScenario (scenario : Json) (requiredId : String) (requiredFrames : Nat) :
    Except String Nat := do
  require ((← scenario.getObjValAs? String "id") == requiredId) "scenario id/order"
  let frames ← arrayField scenario "frames"
  require (frames.size == requiredFrames) "missing mandatory frame"
  let mut state : ModelMap := []
  let mut known : List Bytes := []
  let mut history : Array (Nat × ModelMap) := #[]
  let mut checkedCount := 0
  for index in List.range frames.size do
    let frame ← expectSome frames[index]? "frame index outside corpus"
    let frameId := s!"{requiredId}/{index}"
    require ((← frame.getObjValAs? String "id") == frameId) "frame id/order"
    let result : Except String (ModelMap × List Bytes × Nat) := do
      let pages ← parsePages frame
      let root ← natField frame "root"
      let mutations ← parseMutations frame
      let expected := applyBatch state mutations
      let checked ← checkRawTree pages root expected
      -- Use the certificate's tree so the runtime path consumes the checked object.
      require (flatten depthBudget checked.tree == expected) "certificate content"
      let nextKnown := (known ++ mutations.map Prod.fst).eraseDups
      checkLookups frame expected nextKnown
      checkScans frame expected
      let retained ← arrayField frame "retained_roots"
      require (retained.size == history.size) "missing retained COW root"
      let mut checkedRoots : List Nat := []
      for snapshot in retained do
        let previous ← natField snapshot "frame"
        require (!checkedRoots.contains previous) "duplicate retained root"
        let (oldRoot, oldState) ← expectSome history[previous]? "unknown retained frame"
        require ((← natField snapshot "root") == oldRoot) "retained root identity mismatch"
        discard <| (checkRawTree pages oldRoot oldState).mapError
          (s!"retained frame {previous}: {·}")
        checkedRoots := previous :: checkedRoots
      return (expected, nextKnown, root)
    let (expected, nextKnown, root) ← result.mapError (s!"{frameId}: {·}")
    state := expected
    known := nextKnown
    history := history.push (root, state)
    checkedCount := checkedCount + 1
  return checkedCount

/-- Validate the corpus schema, scenario set and required frame counts. -/
def checkCorpus (json : Json) : Except String Nat := do
  require ((← natField json "schema_version") == 1) "unsupported schema version"
  let mode ← json.getObjValAs? String "mode"
  let required ← if mode == "full" then pure [("long-cow", 6), ("prefix-keys", 4), ("empty-root", 5)]
    else if mode == "witness" then pure [("separator-witness", 1)]
    else throw "unsupported corpus mode"
  let scenarios ← arrayField json "scenarios"
  require (scenarios.size == required.length) "missing mandatory scenario"
  let mut count := 0
  for index in List.range required.length do
    let (id, frameCount) ← expectSome required[index]? "scenario index outside requirements"
    let scenario ← expectSome scenarios[index]? "scenario index outside corpus"
    count := count + (← checkScenario scenario id frameCount)
  return count

end BTreePageConformance

/-- Run this module's command-line entry point. -/
def main (args : List String) : IO UInt32 := do
  let stderr ← IO.getStderr
  match args with
  | [path] =>
      try
        let source ← IO.FS.readFile path
        if source.utf8ByteSize > 32 * 1024 * 1024 then
          stderr.putStrLn "B-tree corpus exceeds 32 MiB"
          return (1 : UInt32)
        match Json.parse source >>= BTreePageConformance.checkCorpus with
        | .error error =>
            stderr.putStrLn error
            return (1 : UInt32)
        | .ok count =>
            (← IO.getStdout).putStrLn s!"Lean B-tree conformance: {count} frames accepted"
            return (0 : UInt32)
      catch error =>
        stderr.putStrLn s!"B-tree corpus: {error}"
        return (1 : UInt32)
  | _ =>
      stderr.putStrLn "usage: check_btree_pages <corpus.json>"
      return (2 : UInt32)
