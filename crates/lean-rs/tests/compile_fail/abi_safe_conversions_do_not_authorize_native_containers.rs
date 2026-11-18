use lean_rs::abi::traits::{IntoLean, TryFromLean};
use lean_rs::{LeanAbi, LeanResult, LeanRuntime, Obj};

struct CustomValue;

impl<'lean> IntoLean<'lean> for CustomValue {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        String::new().into_lean(runtime)
    }
}

impl<'lean> TryFromLean<'lean> for CustomValue {
    fn try_from_lean(_obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self)
    }
}

fn native<'lean, T: LeanAbi<'lean>>() {}

fn main() {
    native::<Vec<CustomValue>>();
    native::<Option<CustomValue>>();
    native::<(CustomValue, CustomValue)>();
}
