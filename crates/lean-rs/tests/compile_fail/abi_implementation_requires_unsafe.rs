use lean_rs::{LeanAbi, LeanResult, LeanRuntime};

struct NativeByte(u8);

impl<'lean> LeanAbi<'lean> for NativeByte {
    type CRepr = u8;

    fn into_c(self, _runtime: &'lean LeanRuntime) -> u8 {
        self.0
    }

    unsafe fn from_c(value: u8, _runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        Ok(Self(value))
    }
}

fn main() {}
