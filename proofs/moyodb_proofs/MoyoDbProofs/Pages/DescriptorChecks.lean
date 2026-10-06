import MoyoDbProofs.Pages.Decode

namespace MoyoDbProofs.Pages

/-- Preserve the exact diagnostic while making finite cases decidable by the kernel. -/
def externalDescriptorDiagnostic (firstPage totalLength : Nat) (value : Model.Bytes) :
    Option String :=
  match checkExternalDescriptor firstPage totalLength value with
  | .ok () => none
  | .error message => some message

/-- Valid bodies and metadata prefixes include the last addressable one-page extent. -/
theorem external_descriptor_accepts_body_and_prefix :
    externalDescriptorDiagnostic 1 1 [1, 0, 0, 0, 0, 0, 0, 0] = none ∧
    externalDescriptorDiagnostic 23 1026 [0, 4, 0, 0, 255, 255, 255, 255, 84, 1] = none ∧
    externalDescriptorDiagnostic 4503599627370495 1 [1, 0, 0, 0, 0, 0, 0, 0] = none := by
  decide +kernel

/-- Short descriptors and invalid stored body lengths fail before extent traversal. -/
theorem external_descriptor_rejects_short_or_empty_body :
    externalDescriptorDiagnostic 1 1 [1, 0, 0, 0, 0, 0, 0] =
      some "short external payload descriptor" ∧
    externalDescriptorDiagnostic 1 0 [0, 0, 0, 0, 0, 0, 0, 0] =
      some "invalid external payload length" ∧
    externalDescriptorDiagnostic 1 8389952 [64, 5, 128, 0, 0, 0, 0, 0] =
      some "invalid external payload length" := by
  decide +kernel

/-- Descriptor body and prefix lengths must equal the bounded logical length. -/
theorem external_descriptor_rejects_length_mismatch :
    externalDescriptorDiagnostic 1 2 [1, 0, 0, 0, 0, 0, 0, 0] =
      some "external payload logical length mismatch" ∧
    externalDescriptorDiagnostic 1 8389952 [63, 5, 128, 0, 0, 0, 0, 0, 1] =
      some "external payload logical length overflow" := by
  decide +kernel

/-- Zero and overflowing page or byte addresses cannot form external extents. -/
theorem external_descriptor_rejects_invalid_extent :
    externalDescriptorDiagnostic 0 1 [1, 0, 0, 0, 0, 0, 0, 0] =
      some "invalid external payload page id" ∧
    externalDescriptorDiagnostic 18446744073709551615 1 [1, 0, 0, 0, 0, 0, 0, 0] =
      some "external payload page range overflow" ∧
    externalDescriptorDiagnostic 4503599627370496 1 [1, 0, 0, 0, 0, 0, 0, 0] =
      some "external payload byte range overflow" := by
  decide +kernel

/-- A raw PAG1 leaf containing one kind-3 descriptor and no PAY2 body. -/
def externalDescriptorPage : RawPage := {
  id := 1
  bytes := [80, 65, 71, 49, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    1, 0, 1, 0, 36, 0, 227, 15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 227, 15] ++
    List.replicate 4031 0 ++
    [1, 0, 3, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0,
      97, 1, 0, 0, 0, 0, 0, 0, 0] }

/-- Raw leaf parsing accepts a valid external descriptor without a payload-page input. -/
theorem external_page_structure_is_accepted :
    (match decodePage externalDescriptorPage with
    | .ok page => page.cells.length == 1 && page.cells.all (fun cell => cell.external)
    | .error _ => false) = true := by
  decide +kernel

/-- Descriptor bytes cannot be certified as logical user-value contents. -/
theorem external_contents_are_not_certified :
    (match interpretRawTree [externalDescriptorPage] 1 with
    | .ok _ => none
    | .error message => some message) = some "external payload contents are outside this checker" := by
  decide +kernel

end MoyoDbProofs.Pages
