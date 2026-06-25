import Lake
open Lake DSL

package moyodb_proofs where
  builtinLint := true

lean_lib MoyoDbProofs

@[default_target]
lean_exe moyodb_proofs where
  root := `MoyoDbProofs.Executable

lean_exe export_artifacts where
  root := `ExportArtifacts
