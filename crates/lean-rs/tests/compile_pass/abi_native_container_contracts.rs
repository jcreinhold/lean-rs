use lean_rs::abi::traits::LeanBoxedAbi;
use lean_rs::host_bridge::{HostEnvironment, HostEvidence};
use lean_rs::{LeanAbi, LeanDeclaration, LeanExpr, LeanLevel, LeanName};

fn native<'lean, T: LeanBoxedAbi<'lean>>() {}
fn direct<'lean, T: LeanAbi<'lean>>() {}

fn main() {
    native::<Vec<Option<LeanExpr<'static>>>>();
    native::<Result<LeanDeclaration<'static>, String>>();
    native::<(LeanName<'static>, Option<LeanLevel<'static>>)>();
    native::<HostEnvironment<'static>>();
    native::<HostEvidence<'static>>();
    direct::<Vec<Option<LeanExpr<'static>>>>();
    direct::<(LeanName<'static>, LeanExpr<'static>)>();
}
