//! Admission for the repository's scalar, String and Array String profiling fixture.
use lean_rs::{LeanRuntime, module::LeanLibrary};
use lean_toolchain::shared_reader::SharedReader;
use std::{
    io,
    path::{Path, PathBuf},
};
#[path = "fixture_inputs.rs"]
mod inputs;
/// The trusted profiling launcher pins the actual runtime, its dependencies and
/// the reader helper before exec and keeps its toolchain stable through exit.
pub(super) struct ScalarFixture {
    image: PathBuf,
    _readers: Vec<SharedReader>,
}

impl ScalarFixture {
    pub(super) fn admit() -> io::Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/lean")
            .canonicalize()?;
        let readers = inputs::hold(std::slice::from_ref(&root))?;
        let lib = root.join(".lake/build/lib");
        let ext = std::env::consts::DLL_EXTENSION;
        let modern = lib.join(format!("liblean__rs__fixture_LeanRsFixture.{ext}"));
        let image = if modern.exists() {
            modern
        } else {
            lib.join(format!("libLeanRsFixture.{ext}"))
        };
        if image.canonicalize()? != image {
            return Err(io::Error::other("noncanonical fixture image"));
        }
        inputs::check_digest(&image, option_env!("LEAN_RS_PROFILE_FIXTURE_SHA256"))?;
        Ok(Self {
            image,
            _readers: readers,
        })
    }

    #[allow(
        dead_code,
        reason = "standalone profiling binaries use this entry; unit tests share open_owned"
    )]
    pub(super) fn open(self, runtime: &'static LeanRuntime) -> lean_rs::LeanResult<&'static LeanLibrary<'static>> {
        let library = Box::leak(Box::new(self)).open_owned(runtime)?;
        Ok(Box::leak(Box::new(library)))
    }

    pub(super) fn open_owned(
        &'static self,
        runtime: &'static LeanRuntime,
    ) -> lean_rs::LeanResult<LeanLibrary<'static>> {
        #[allow(
            unsafe_code,
            reason = "the profiling launcher and retained fixture admission cover this exact image"
        )]
        // SAFETY: the approved hash identifies the controlled fixture and its initializer.
        // The launcher pins its runtime closure. The reader and loaded handle survive until exit.
        unsafe {
            LeanLibrary::open(runtime, &self.image)
        }
    }
}
