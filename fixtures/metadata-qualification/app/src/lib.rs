#![cfg(feature = "admitted")]
use lean_toolchain::shared_writer::project_lock_path;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CStr,
    fs,
    io::{self, Read, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

pub const EXPORT: &str = "lean_rs_interop_consumer_worker_metadata";
pub const MODULE: &str = "LeanRsInteropConsumer.Callback";
type R<T> = io::Result<T>;
fn err(s: impl Into<String>) -> io::Error {
    io::Error::other(s.into())
}
fn require(ok: bool, detail: &str) -> R<()> {
    if ok { Ok(()) } else { Err(err(detail)) }
}
#[derive(Deserialize)]
pub struct Record {
    pub typed_session: bool,
    pub consumer_image: PathBuf,
    pub consumer_manifest: PathBuf,
    pub consumer_json: String,
    pub shim_manifest: PathBuf,
    pub shim_json: String,
    pub shim_paths: Vec<PathBuf>,
    pub shim_roots: Vec<PathBuf>,
    pub interop_image: PathBuf,
    pub host_image: PathBuf,
    pub project: PathBuf,
    pub roots: Vec<PathBuf>,
    pub import_roots: Vec<PathBuf>,
    pub ordered_directory_ids: Vec<(u64, u64)>,
    pub sysroot: PathBuf,
    files: BTreeMap<PathBuf, (u64, u64, String)>,
    directories: BTreeSet<PathBuf>,
}
impl Record {
    pub fn selected() -> Self {
        serde_json::from_slice(include_bytes!("../deployment.json"))
            .expect("build-selected fixture record")
    }
    pub fn helper() -> PathBuf {
        PathBuf::from(env!("D3_HELPER"))
    }
    pub fn descriptor(&self) -> lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts {
        serde_json::from_value(serde_json::json!({"protocol":1,"shim_manifest_path":self.shim_manifest,"shim_manifest_json":self.shim_json,"artifact_roots":self.shim_roots,"shim_search_paths":self.shim_paths})).unwrap()
    }
    fn check_file(&self, path: &Path) -> R<()> {
        let (dev, ino, digest) = self.files.get(path).ok_or_else(|| err("unselected file"))?;
        require(path.canonicalize()? == path, "aliased deployment file")?;
        let mut f = fs::File::open(path)?;
        let meta = f.metadata()?;
        require(
            meta.is_file() && (*dev, *ino) == (meta.dev(), meta.ino()),
            &format!("wrong file identity {}", path.display()),
        )?;
        let mut hash = Sha256::new();
        let mut buf = vec![0; 128 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        require(
            &hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                == digest,
            &format!("wrong file digest {}", path.display()),
        )
    }
    pub fn check_order(
        &self,
        paths: &[String],
        names: &[String],
        options: &lean_rs::module::HostImportOptions,
    ) -> R<()> {
        let expected: &[&str] = if self.typed_session {
            &["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"]
        } else {
            &[MODULE]
        };
        require(
            names == expected && *options == Default::default(),
            "wrong module or import profile",
        )?;
        require(
            paths
                .iter()
                .map(Path::new)
                .eq(self.import_roots.iter().map(PathBuf::as_path)),
            "wrong ordered import roots",
        )
    }
}

pub fn audit(event: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("native-events.log");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{} {event}", std::process::id()).unwrap();
}
pub struct Held {
    pub record: Record,
    _locks: Vec<fs::File>,
}
impl Held {
    pub fn acquire(record: Record) -> R<Self> {
        let mut locks = record
            .roots
            .iter()
            .map(|r| project_lock_path(r))
            .collect::<R<Vec<_>>>()?;
        locks.sort();
        let admission = lean_toolchain::writer_demand::ReadAdmission::try_acquire(&locks)?
            .ok_or_else(|| err("competing writer"))?;
        locks.push(record.sysroot.parent().unwrap().join("release.lock"));
        let mut held = vec![];
        for p in locks {
            require(p.canonicalize()? == p, "aliased custody domain")?;
            let f = fs::File::open(&p)?;
            f.try_lock_shared().map_err(io::Error::from)?;
            let a = f.metadata()?;
            let b = fs::symlink_metadata(p)?;
            require(
                b.is_file() && (a.dev(), a.ino()) == (b.dev(), b.ino()),
                "changed lock identity",
            )?;
            held.push(f);
        }
        drop(admission);
        Ok(Self {
            record,
            _locks: held,
        })
    }
    pub fn check_interop_visibility(&self, loaded: bool) -> R<()> {
        // SAFETY: the symbol literal is NUL-terminated. This only inspects dyld's
        // default scope. No foreign function is invoked. The process pins every
        // initialized image, and this application holds its actual files.
        unsafe {
            let address = libc::dlsym(
                libc::RTLD_DEFAULT,
                c"initialize_lean__rs__interop__shims_LeanRsInterop".as_ptr(),
            );
            if !loaded {
                return require(
                    address.is_null(),
                    "interop initializer already globally visible",
                );
            }
            require(
                !address.is_null(),
                "interop initializer missing from global scope",
            )?;
            let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
            require(
                libc::dladdr(address, info.as_mut_ptr()) != 0,
                "interop initializer has no loaded image",
            )?;
            let info = info.assume_init();
            require(
                !info.dli_fname.is_null(),
                "missing loaded interop image path",
            )?;
            let actual = Path::new(std::ffi::OsStr::from_bytes(
                CStr::from_ptr(info.dli_fname).to_bytes(),
            ));
            require(
                actual.canonicalize()? == self.record.interop_image,
                "wrong mapped global interop initializer",
            )?;
        }
        Ok(())
    }
    pub fn verify(&self) -> R<()> {
        let d = &self.record;
        require(
            std::env::var_os("LEAN_SYSROOT").as_deref() == Some(d.sysroot.as_os_str())
                && std::env::var_os("LEAN_PATH").is_none()
                && !std::env::vars_os().any(|(key, _)| key.as_bytes().starts_with(b"DYLD_")),
            "unexpected runtime selection",
        )?;
        let libs = d.sysroot.join("lib/lean");
        let mut found = false;
        // SAFETY: before this application loads its first native capability, Darwin supplies
        // the current image count and NUL-terminated names. No application image is unloaded.
        unsafe {
            for i in 0..mach2::dyld::_dyld_image_count() {
                let ptr = mach2::dyld::_dyld_get_image_name(i);
                if ptr.is_null() {
                    return Err(err("missing dyld image name"));
                }
                let path = Path::new(std::ffi::OsStr::from_bytes(CStr::from_ptr(ptr).to_bytes()));
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("libleanshared")
                    || name == "libInit_shared.dylib"
                    || name == "libLake_shared.dylib"
                {
                    require(
                        path.canonicalize()? == libs.join(name.as_ref()),
                        "unexpected mapped Lean runtime",
                    )?;
                    found |= name == "libleanshared.dylib";
                }
                if name.contains("LeanRsInterop")
                    || name.contains("LeanRsHostShims")
                    || name.contains("LeanRsFixture")
                {
                    return Err(err("preloaded application capability"));
                }
            }
        }
        require(found, "selected libleanshared absent")?;
        require(
            fs::read_to_string(&d.consumer_manifest)? == d.consumer_json,
            "consumer manifest or initializer differs",
        )?;
        require(
            fs::read_to_string(&d.shim_manifest)? == d.shim_json,
            "host manifest or initializer differs",
        )?;
        require(
            d.import_roots.len() == d.ordered_directory_ids.len(),
            "incomplete ordered root identity",
        )?;
        for (path, id) in d.import_roots.iter().zip(&d.ordered_directory_ids) {
            let dir = fs::symlink_metadata(path)?;
            require(
                dir.is_dir() && (dir.dev(), dir.ino()) == *id,
                &format!("wrong ordered import directory identity {}", path.display()),
            )?;
        }
        d.check_file(&d.interop_image)?;
        d.check_file(&d.consumer_image)?;
        d.check_file(&d.host_image)?;
        for path in d.files.keys() {
            d.check_file(path)?;
        }
        for dir in &d.directories {
            require(
                dir.canonicalize()? == *dir && dir.is_dir(),
                "import directory changed",
            )?;
            for child in fs::read_dir(dir)? {
                let p = child?.path();
                require(
                    d.files.contains_key(&p) || d.directories.contains(&p),
                    "new entry in selected import tree",
                )?;
            }
        }
        self.check_interop_visibility(false)?;
        audit("verified-deployment");
        Ok(())
    }
}
