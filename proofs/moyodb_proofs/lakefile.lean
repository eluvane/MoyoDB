import Lake
open Lake DSL

package moyodb_proofs where
  builtinLint := true
  lintDriver := "lintLean"
  leanOptions := #[
    ⟨`autoImplicit, false⟩,
    ⟨`warningAsError, true⟩,
    ⟨`linter.all, true⟩,
    ⟨`linter.extra, true⟩]

require heron from git "https://github.com/wvhulle/heron" @
  "1b8526c644a49572a0a63f1176fc5ac2e1c96eea"
require JunkLinter from git "https://github.com/junk-values/junk-value-linter" @
  "bf2e7937c9a327f7d7b075c18830423c332b4d4b"
require batteries from git "https://github.com/leanprover-community/batteries" @
  "fa08db58b30eb033edcdab331bba000827f9f785"

/-- Run the pinned external linters after Lake's built-in checks. -/
script lintLean do
  let child ← IO.Process.spawn {
    cmd := "node"
    args := #["../../.config/moyo/quality/check-lean-linters.mjs"] }
  child.wait

lean_lib MoyoDbProofs

lean_lib ProofTools where
  roots := #[`CheckBTreePages, `ExportArtifacts]

lean_lib ProofScripts where
  roots := #[`scripts.ExportArtifacts]

@[default_target]
lean_exe moyodb_proofs where
  root := `MoyoDbProofs.Executable

lean_exe export_artifacts where
  root := `ExportArtifacts

lean_exe check_btree_pages where
  root := `CheckBTreePages
