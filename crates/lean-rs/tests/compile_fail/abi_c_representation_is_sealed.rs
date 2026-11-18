use lean_rs::LeanCReprAbi;
use lean_rs::module::LeanExportAbiRepr;

#[derive(Clone, Copy)]
struct ForgedRepresentation([u8; 32]);

impl LeanCReprAbi for ForgedRepresentation {
    const EXPORT_ABI_REPR: LeanExportAbiRepr = LeanExportAbiRepr::LeanObject;
}

fn main() {}
