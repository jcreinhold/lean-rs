use lean_rs::abi::traits::LeanAbi;
use lean_rs::{DecodeCallResult, LeanIo, LeanRuntime, Obj};

fn raw_decode<'lean>(runtime: &'lean LeanRuntime, raw: <Obj<'lean> as LeanAbi<'lean>>::CRepr) {
    let _ = Obj::from_c(raw, runtime);
    let _ = <Obj<'lean> as DecodeCallResult<'lean>>::decode_c(raw, runtime);
    let _ = <LeanIo<Obj<'lean>> as DecodeCallResult<'lean>>::decode_c(raw, runtime);
}

fn main() {}
