use lean_rs::abi::traits::TryFromLean;
use lean_rs::host_bridge::{HostEnvironment, HostEvidence};
use lean_rs::{LeanDeclaration, LeanExpr, LeanLevel, LeanName};

fn safe_decoder<'lean, T: TryFromLean<'lean>>() {}

fn main() {
    safe_decoder::<LeanName<'static>>();
    safe_decoder::<LeanExpr<'static>>();
    safe_decoder::<LeanLevel<'static>>();
    safe_decoder::<LeanDeclaration<'static>>();
    safe_decoder::<HostEnvironment<'static>>();
    safe_decoder::<HostEvidence<'static>>();
}
