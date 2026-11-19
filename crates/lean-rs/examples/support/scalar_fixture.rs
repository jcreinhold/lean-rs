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
    #[cfg(all(test, unix))]
    _private_image: Option<std::fs::File>,
}

impl ScalarFixture {
    pub(super) fn admit() -> io::Result<Self> {
        // A trusted test job may select its private project and exact image identity.
        #[cfg(all(test, unix))]
        let selected: Option<(PathBuf, u64, u64)> = option_env!("LEAN_RS_TEST_SCALAR_FIXTURE")
            .map(serde_json::from_str)
            .transpose()?;
        #[cfg(not(all(test, unix)))]
        let selected: Option<(PathBuf, u64, u64)> = None;
        let root = match &selected {
            Some((root, _, _)) => {
                if root.canonicalize()? != *root {
                    return Err(io::Error::other("noncanonical selected scalar project"));
                }
                root.clone()
            }
            None => Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/lean")
                .canonicalize()?,
        };
        #[cfg(all(test, unix))]
        let private_job = option_env!("LEAN_RS_TEST_SCALAR_PRIVATE_JOB").is_some()
            || std::env::var_os("LEAN_RS_TEST_SCALAR_PRIVATE_JOB").is_some();
        #[cfg(not(all(test, unix)))]
        let private_job = false;
        #[cfg(all(test, unix))]
        if private_job {
            use std::os::unix::fs::MetadataExt as _;
            let temporary = std::env::temp_dir().canonicalize()?;
            let metadata = root.metadata()?;
            let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
            let checkout_fixture = checkout.join("fixtures/lean");
            let owned_location = (root == checkout_fixture && metadata.uid() == checkout.metadata()?.uid())
                || (root.parent() == Some(temporary.as_path()) && metadata.uid() == temporary.metadata()?.uid());
            if option_env!("LEAN_RS_TEST_SCALAR_PRIVATE_JOB") != Some("1")
                || std::env::var("LEAN_RS_TEST_SCALAR_PRIVATE_JOB").as_deref() != Ok("1")
                || selected.is_none()
                || !owned_location
                || metadata.mode() & 0o7777 != 0o700
            {
                return Err(io::Error::other(
                    "fixture is not owned by the selected private test job",
                ));
            }
            inputs::check_prefix()?;
        }
        // Private jobs run build then tests serially and retain the directory until exit.
        // This is trusted job ownership, not a lock against arbitrary same-user writes.
        let readers = if private_job {
            Vec::new()
        } else {
            inputs::hold(std::slice::from_ref(&root))?
        };
        let lib = root.join(".lake/build/lib");
        let ext = std::env::consts::DLL_EXTENSION;
        let modern = lib.join(format!("liblean__rs__fixture_LeanRsFixture.{ext}"));
        let image = if selected.is_some() || modern.exists() {
            modern
        } else {
            lib.join(format!("libLeanRsFixture.{ext}"))
        };
        if image.canonicalize()? != image {
            return Err(io::Error::other("noncanonical fixture image"));
        }
        #[cfg(all(test, unix))]
        if let Some((_, device, inode)) = &selected {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = image.metadata()?;
            if (metadata.dev(), metadata.ino()) != (*device, *inode) {
                return Err(io::Error::other("selected scalar fixture image identity changed"));
            }
        }
        if !private_job {
            inputs::check_digest(&image, option_env!("LEAN_RS_PROFILE_FIXTURE_SHA256"))?;
        }
        #[cfg(all(test, unix))]
        let private_image = if private_job {
            let (_, device, inode) = selected.ok_or_else(|| io::Error::other("missing private image selection"))?;
            let digest = option_env!("LEAN_RS_PROFILE_FIXTURE_SHA256")
                .ok_or_else(|| io::Error::other("missing private image digest"))?;
            Some(inputs::open_selected(&image, device, inode, digest)?)
        } else {
            None
        };
        Ok(Self {
            image,
            _readers: readers,
            #[cfg(all(test, unix))]
            _private_image: private_image,
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
        // The launcher pins its runtime closure. Readers or exclusive private-job ownership
        // cover the image until process exit; the retained file alone is not a writer lock.
        unsafe {
            LeanLibrary::open(runtime, &self.image)
        }
    }
}
