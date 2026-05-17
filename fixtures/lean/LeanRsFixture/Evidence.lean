/-! ABI category: a value carrying a `Prop` witness. The proof field is
    runtime-irrelevant (`lean_box(0)` placeholder), but the structure as a whole
    is the simplest fixture for `SEMANTIC-HANDLES`: Rust holds the returned
    `lean_object*` opaquely and never inspects the witness. -/

namespace LeanRsFixture.Evidence

structure EvidenceCarrier where
  marker  : Nat
  witness : marker = 42

@[export lean_rs_fixture_evidence_carrier]
def evidenceCarrier : EvidenceCarrier where
  marker  := 42
  witness := rfl

end LeanRsFixture.Evidence
