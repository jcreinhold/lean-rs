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
}
impl CallbackImages {
    fn admit() -> Result<Self, Box<dyn std::error::Error>> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
        let interop = workspace.join("crates/lean-rs/shims/lean-rs-interop-shims");
        let consumer = workspace.join("fixtures/interop-shims");
        let readers = inputs::hold(&[interop.clone(), consumer.clone()])?;
        fn image(root: &Path, new: &str, old: &str, pin: Option<&str>) -> io::Result<PathBuf> {
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
            inputs::check_digest(&path, pin)?;
            Ok(path)
        }
        Ok(Self {
            interop: image(
                &interop,
                "liblean__rs__interop__shims_LeanRsInterop",
                "libLeanRsInterop",
                option_env!("LEAN_RS_INTEROP_IMAGE_SHA256"),
            )?,
            consumer: image(
                &consumer,
                "liblean__rs__interop__consumer_LeanRsInteropConsumer",
                "libLeanRsInteropConsumer",
                option_env!("LEAN_RS_CALLBACK_CONSUMER_SHA256"),
            )?,
            _readers: readers,
        })
    }
    fn dependency(&self) -> LeanLibraryDependency {
        LeanLibraryDependency::path(&self.interop)
            .export_symbols_for_dependents()
            .initializer("lean_rs_interop_shims", "LeanRsInterop")
    }
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
