import ExportArtifacts

namespace ProofArtifactScript

/-- Legacy script entry point forwarding to the project's artifact writer. -/
def main : IO Unit := writeArtifacts

end ProofArtifactScript
