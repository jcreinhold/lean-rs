//! Only the checked-in consumer and interop dependency belong to this loader.
use lean_rs::{
    LeanRuntime,
    module::{LeanLibraryBundle, LeanLibraryDependency},
};
use lean_toolchain::shared_reader::SharedReader;
use std::{
    io,
    path::{Path, PathBuf},
};
#[path = "fixture_inputs.rs"]
mod inputs;

struct CallbackImages {
    consumer: PathBuf,
    interop: PathBuf,
    _readers: Vec<SharedReader>,
    #[cfg(all(test, unix))]
    _private_images: Vec<std::fs::File>,
}
impl CallbackImages {
    fn admit() -> Result<Self, Box<dyn std::error::Error>> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
        let interop = workspace.join("crates/lean-rs/shims/lean-rs-interop-shims");
        let consumer = workspace.join("fixtures/interop-shims");
        #[cfg(all(test, unix))]
        let private_job = private_mode(
            option_env!("LEAN_RS_TEST_CALLBACK_PRIVATE_JOB"),
            std::env::var_os("LEAN_RS_TEST_CALLBACK_PRIVATE_JOB").as_deref(),
        )?;
        #[cfg(not(all(test, unix)))]
        let private_job = false;
        // The trusted job builds both projects serially, then keeps them unchanged
        // until tests exit. Open files are not locks against same-user writers.
        let readers = if private_job {
            inputs::check_prefix()?;
            Vec::new()
        } else {
            inputs::hold(&[interop.clone(), consumer.clone()])?
        };
        fn image(root: &Path, new: &str, old: &str) -> io::Result<PathBuf> {
            let dir = root.join(".lake/build/lib");
            let ext = std::env::consts::DLL_EXTENSION;
            let modern = dir.join(format!("{new}.{ext}"));
            let path = if modern.exists() {
                modern
            } else {
                dir.join(format!("{old}.{ext}"))
            };
            if path.canonicalize()? != path {
                return Err(io::Error::other("noncanonical callback fixture path"));
            }
            Ok(path)
        }
        let interop_image = image(
            &interop,
            "liblean__rs__interop__shims_LeanRsInterop",
            "libLeanRsInterop",
        )?;
        let consumer_image = image(
            &consumer,
            "liblean__rs__interop__consumer_LeanRsInteropConsumer",
            "libLeanRsInteropConsumer",
        )?;
        #[cfg(all(test, unix))]
        let mut private_images = Vec::new();
        for (root, path, digest, selection) in [
            (
                &interop,
                &interop_image,
                option_env!("LEAN_RS_INTEROP_IMAGE_SHA256"),
                option_env!("LEAN_RS_TEST_CALLBACK_INTEROP"),
            ),
            (
                &consumer,
                &consumer_image,
                option_env!("LEAN_RS_CALLBACK_CONSUMER_SHA256"),
                option_env!("LEAN_RS_TEST_CALLBACK_CONSUMER"),
            ),
        ] {
            #[cfg(all(test, unix))]
            if private_job {
                use std::os::unix::fs::MetadataExt as _;
                let (selected, device, inode): (PathBuf, u64, u64) = serde_json::from_str(
                    selection.ok_or_else(|| io::Error::other("missing private callback selection"))?,
                )?;
                let metadata = root.metadata()?;
                if selected != *path
                    || root.canonicalize()? != *root
                    || metadata.uid() != workspace.metadata()?.uid()
                    || metadata.mode() & 0o7777 != 0o700
                {
                    return Err(io::Error::other("callback fixture is not owned by the selected test job").into());
                }
                private_images.push(inputs::open_selected(
                    path,
                    device,
                    inode,
                    digest.ok_or_else(|| io::Error::other("missing private callback digest"))?,
                )?);
                continue;
            }
            #[cfg(not(all(test, unix)))]
            let _ = (root, selection);
            inputs::check_digest(path, digest)?;
        }
        Ok(Self {
            interop: interop_image,
            consumer: consumer_image,
            _readers: readers,
            #[cfg(all(test, unix))]
            _private_images: private_images,
        })
    }
    fn dependency(&self) -> LeanLibraryDependency {
        LeanLibraryDependency::path(&self.interop)
            .export_symbols_for_dependents()
            .initializer("lean_rs_interop_shims", "LeanRsInterop")
    }
}
#[cfg(all(test, unix))]
fn private_mode(compiled: Option<&str>, runtime: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match (compiled, runtime) {
        (None, None) => Ok(false),
        (Some("1"), Some(value)) if value == "1" => Ok(true),
        _ => Err(io::Error::other("invalid private callback job marker")),
    }
}

#[cfg(all(test, unix))]
#[test]
fn private_callback_mode_requires_both_markers() {
    use std::ffi::OsStr;
    assert_eq!(private_mode(None, None).ok(), Some(false));
    assert_eq!(private_mode(Some("1"), Some(OsStr::new("1"))).ok(), Some(true));
    assert!(private_mode(Some("1"), None).is_err());
    assert!(private_mode(None, Some(OsStr::new("1"))).is_err());
    assert!(private_mode(Some("0"), Some(OsStr::new("1"))).is_err());
    assert!(private_mode(Some("1"), Some(OsStr::new("invalid"))).is_err());
}

pub(super) fn bundle() -> Result<&'static LeanLibraryBundle<'static>, Box<dyn std::error::Error>> {
    let images = CallbackImages::admit()?;
    let runtime = LeanRuntime::init()?;
    #[allow(unsafe_code, reason = "restricted to the approved callback fixture pair")]
    // SAFETY: both checked image hashes cover the source-defined dependency ordering,
    // global visibility and initializer. The launcher pins the runtime closure through exit.
    let bundle = unsafe { LeanLibraryBundle::open(runtime, &images.consumer, [images.dependency()]) }?;
    Box::leak(Box::new(images));
    Ok(Box::leak(Box::new(bundle)))
}
