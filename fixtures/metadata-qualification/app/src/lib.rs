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

pub const MODULE_SYNTAX_IMPORTS: [&str; 3] = ["Fixture.Imported", "Fixture.Internal", "Fixture.PrivateScope"];

pub fn module_syntax_case() -> bool {
    std::env::var_os("LEAN_RS_TEST_MODULE_SYNTAX_CASE")
        .is_some_and(|name| name == "process_module_query_handles_module_system_header_through_worker")
}

struct MappedImage {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    device: Option<(u32, u32)>,
    #[cfg(target_os = "linux")]
    inode: Option<u64>,
}

#[cfg(target_os = "linux")]
fn parse_linux_maps(maps: &[u8]) -> R<Vec<MappedImage>> {
    let mut paths = BTreeMap::new();
    for line in maps.split(|byte| *byte == b'\n') {
        let mut cursor = 0;
        let mut fields = Vec::with_capacity(5);
        for _ in 0..5 {
            while line.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            let start = cursor;
            while line.get(cursor).is_some_and(|byte| !byte.is_ascii_whitespace()) {
                cursor += 1;
            }
            if start == cursor {
                break;
            }
            fields.push(&line[start..cursor]);
        }
        if fields.len() != 5 {
            continue;
        }
        while line.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let raw_path = &line[cursor..];
        if !raw_path.starts_with(b"/") {
            continue;
        }
        // proc maps preserves ordinary spaces and tabs but renders a newline in a
        // pathname as the ambiguous bytes `\012`. This fixture never selects such
        // a path, so refuse it rather than guessing whether it was literal.
        require(
            !raw_path.windows(4).any(|part| part == b"\\012"),
            "ambiguous mapped image path",
        )?;
        require(!raw_path.ends_with(b" (deleted)"), "mapped image was deleted")?;
        let device = std::str::from_utf8(fields[3])
            .map_err(|_| err("invalid mapped image device"))?
            .split_once(':')
            .ok_or_else(|| err("invalid mapped image device"))?;
        let device = (
            u32::from_str_radix(device.0, 16).map_err(|_| err("invalid mapped image device"))?,
            u32::from_str_radix(device.1, 16).map_err(|_| err("invalid mapped image device"))?,
        );
        let inode = std::str::from_utf8(fields[4])
            .map_err(|_| err("invalid mapped image inode"))?
            .parse::<u64>()
            .map_err(|_| err("invalid mapped image inode"))?;
        require(inode != 0, "mapped file has no inode identity")?;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(raw_path));
        if let Some(previous) = paths.insert(path, (device, inode)) {
            require(previous == (device, inode), "mapped path has conflicting identities")?;
        }
    }
    Ok(paths
        .into_iter()
        .map(|(path, (device, inode))| MappedImage {
            path,
            device: Some(device),
            inode: Some(inode),
        })
        .collect())
}

#[cfg(target_os = "linux")]
fn mapped_images() -> R<Vec<MappedImage>> {
    parse_linux_maps(&fs::read("/proc/self/maps")?)
}

#[cfg(target_os = "macos")]
fn mapped_images() -> R<Vec<MappedImage>> {
    let mut paths = BTreeSet::new();
    // SAFETY: before this application loads its first native capability, Darwin supplies
    // the current image count and NUL-terminated names. No application image is unloaded.
    unsafe {
        for i in 0..mach2::dyld::_dyld_image_count() {
            let ptr = mach2::dyld::_dyld_get_image_name(i);
            require(!ptr.is_null(), "missing dyld image name")?;
            paths.insert(PathBuf::from(std::ffi::OsStr::from_bytes(
                CStr::from_ptr(ptr).to_bytes(),
            )));
        }
    }
    Ok(paths.into_iter().map(|path| MappedImage { path }).collect())
}

fn runtime_image_names() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_os = "macos")]
    return ("libleanshared.dylib", "libInit_shared.dylib", "libLake_shared.dylib");
    #[cfg(target_os = "linux")]
    return ("libleanshared.so", "libInit_shared.so", "libLake_shared.so");
}

pub const EXPORT: &str = "lean_rs_interop_consumer_worker_metadata";
pub const JSON_COMMAND_EXPORT: &str = "lean_rs_interop_consumer_worker_json_command";
pub const STREAM_MANIFEST_NAME: &str = "worker-stream-capability.json";
pub const JSON_MANIFEST_NAME: &str = "worker-json-capability.json";
pub const MODULE: &str = "LeanRsInteropConsumer.Callback";
pub const GENERATED_OUTPUT_KEY: &str = "lean-4.36.0";
type R<T> = io::Result<T>;
fn err(s: impl Into<String>) -> io::Error {
    io::Error::other(s.into())
}
fn require(ok: bool, detail: &str) -> R<()> {
    if ok { Ok(()) } else { Err(err(detail)) }
}
#[derive(Deserialize)]
pub struct Record {
    #[serde(default)]
    pub broken_import: bool,
    #[serde(default)]
    pub host_builder_case: Option<String>,
    pub typed_session: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub signature_probe: Option<String>,
    pub verification: bool,
    pub generated_output_key: String,
    #[serde(default)]
    pub consumer_image: PathBuf,
    #[serde(default)]
    pub consumer_manifest: PathBuf,
    #[serde(default)]
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
    content_files: BTreeMap<PathBuf, String>,
    directories: BTreeSet<PathBuf>,
}
impl Record {
    pub fn selected() -> Self {
        let path = Path::new(
            option_env!("LEAN_RS_METADATA_DEPLOYMENT_RECORD")
                .expect("worker built without a selected deployment record"),
        );
        let bytes = fs::read(path).expect("read build-selected fixture record");
        let actual = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            actual,
            option_env!("LEAN_RS_METADATA_DEPLOYMENT_SHA256")
                .expect("worker built without a selected deployment digest"),
            "build-selected fixture record changed"
        );
        serde_json::from_slice(&bytes).expect("parse build-selected fixture record")
    }
    pub fn helper() -> PathBuf {
        PathBuf::from(option_env!("D3_HELPER").expect("ordinary deployment requires its selected helper"))
    }
    pub fn descriptor(&self) -> lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts {
        serde_json::from_value(serde_json::json!({
            "protocol": 2,
            "shim_manifest_path": self.shim_manifest,
            "shim_manifest_json": self.shim_json,
            "artifact_roots": self.shim_roots,
            "shim_search_paths": self.shim_paths,
        }))
        .unwrap()
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
    fn check_content_file(&self, path: &Path) -> R<()> {
        let digest = self
            .content_files
            .get(path)
            .ok_or_else(|| err("unselected content file"))?;
        require(path.canonicalize()? == path, "aliased deployment content file")?;
        let mut f = fs::File::open(path)?;
        require(f.metadata()?.is_file(), "deployment content path is not a file")?;
        let mut hash = Sha256::new();
        let mut buf = vec![0; 128 * 1024];
        loop {
            let read = f.read(&mut buf)?;
            if read == 0 {
                break;
            }
            hash.update(&buf[..read]);
        }
        let actual = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        require(actual == digest.as_str(), "deployment content file changed")
    }
    pub fn verification_roots(&self) -> Vec<String> {
        self.import_roots
            .iter()
            .chain(std::iter::once(&self.sysroot.join("lib/lean")))
            .map(|p| p.display().to_string())
            .collect()
    }
    pub fn host_builder_import_event(&self, names: &[String]) -> Option<&'static str> {
        match (self.host_builder_case.as_deref(), names) {
            (Some("shims_only_host_handle_imports_transitive_lake_package_oleans"), [name]) => match name.as_str() {
                "Consumer" => Some("authorize-builder-consumer"),
                "Dep.NonExistent" => Some("authorize-builder-missing-dependency"),
                _ => None,
            },
            (Some("a_host_handle_built_without_imports_opens_no_session"), []) => Some("authorize-builder-empty"),
            (Some("shims_only_host_handle_skips_user_shared_build"), [name]) if name == "Demo.Broken" => {
                Some("authorize-builder-broken")
            }
            (
                Some(
                    "a_host_handle_built_without_imports_opens_no_session"
                    | "shims_only_host_handle_skips_user_shared_build",
                ),
                [name],
            ) if name == "Demo.Good" => Some("authorize-builder-good"),
            _ => None,
        }
    }

    pub fn adapter_handles_case(&self) -> bool {
        self.typed_session
            && !self.stream
            && !self.broken_import
            && !self.verification
            && option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1")
            && matches!(
                std::env::var("LEAN_RS_TEST_ADAPTER_CASE").as_deref(),
                Ok("declaration_bulk_adapter_matches_in_process_session"
                    | "progress_events_cross_worker_boundary_in_order"
                    | "parent_can_cancel_long_worker_request_at_progress_boundary"
                    | "max_import_policy_restarts_before_next_import")
            )
    }

    pub fn adapter_kernel_case(&self) -> bool {
        self.typed_session
            && !self.stream
            && !self.broken_import
            && !self.verification
            && option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1")
            && std::env::var("LEAN_RS_TEST_ADAPTER_CASE").as_deref()
                == Ok("elaboration_and_kernel_adapter_match_status_shape")
    }

    pub fn check_order(
        &self,
        paths: &[String],
        names: &[String],
        options: &lean_rs::module::HostImportOptions,
    ) -> R<()> {
        let expected: &[&str] = if self.broken_import {
            &["Demo.Broken"]
        } else if self.typed_session && !self.stream {
            &["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"]
        } else {
            &[MODULE]
        };
        let wide_reuse = names == ["LeanRsHostShims.Elaboration", "LeanRsFixture.Handles"]
            && matches!(
                std::env::var("LEAN_RS_TEST_WIDE_REUSE_CASE").as_deref(),
                Ok("switching_back_to_a_pooled_import_set_does_not_reimport"
                    | "capacity_one_restores_the_pre_pool_child"
                    | "a_mispredicted_reuse_restarts_before_the_actual_import"
                    | "a_reused_session_carries_no_state_from_the_previous_one")
            );
        let staleness_probe = std::env::var_os("LEAN_RS_TEST_STALENESS_PROFILE").is_some_and(|v| v == "1")
            && (names == ["LeanRsHostShims.Elaboration", "LeanRsFixtureLateExtension"]
                || names == ["LeanRsHostShims.Elaboration", "LeanRsFixture.Meta"]);
        let reuse_imports = self.typed_session
            && !self.stream
            && !self.broken_import
            && !self.verification
            && option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1")
            && (wide_reuse
                || staleness_probe
                || (module_syntax_case() && names == MODULE_SYNTAX_IMPORTS)
                || [
                    &["LeanRsHostShims.Elaboration"][..],
                    &["LeanRsHostShims.Elaboration", "Lean"][..],
                    &["LeanRsFixture.Scalars"][..],
                    &["LeanRsFixture.Scalars", "LeanRsFixture.Strings"][..],
                    &["LeanRsFixture.Strings", "LeanRsFixture.Scalars"][..],
                ]
                .iter()
                .any(|allowed| names == *allowed));
        require(
            (if self.host_builder_case.is_some() {
                self.host_builder_import_event(names).is_some()
            } else if std::env::var_os("LEAN_RS_TEST_ADAPTER_CASE").is_some() {
                (self.adapter_handles_case()
                    && (names == ["LeanRsFixture.Handles"]
                        || (names == ["LeanRsHostShims.Elaboration"]
                            && std::env::var("LEAN_RS_TEST_ADAPTER_CASE").as_deref()
                                == Ok("max_import_policy_restarts_before_next_import"))))
                    || (self.adapter_kernel_case() && names == ["LeanRsHostShims.Elaboration"])
            } else {
                names == expected || reuse_imports
            }) && *options == Default::default(),
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
    const MAX_EVENT_BYTES: usize = 2 * 1024;
    const MAX_AUDIT_BYTES: u64 = 16 * 1024;
    let path = Path::new(
        option_env!("LEAN_RS_METADATA_AUDIT_PATH").expect("worker built without a selected bounded audit path"),
    );
    let line = format!("{} {event}\n", std::process::id());
    assert!(
        line.len() <= MAX_EVENT_BYTES,
        "fixture audit event exceeds its byte limit"
    );
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    f.try_lock().expect("fixture audit file has another writer");
    assert!(
        f.metadata().unwrap().len() + u64::try_from(line.len()).unwrap() <= MAX_AUDIT_BYTES,
        "fixture audit exceeds its byte limit"
    );
    f.write_all(line.as_bytes()).unwrap();
    f.unlock().unwrap();
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
        let mut held = vec![];
        for p in locks {
            require(p.canonicalize()? == p, "aliased custody domain")?;
            let f = fs::File::open(&p)?;
            f.try_lock_shared().map_err(io::Error::from)?;
            let a = f.metadata()?;
            let b = fs::symlink_metadata(p)?;
            require(
                b.is_file() && a.nlink() == 1 && (a.dev(), a.ino()) == (b.dev(), b.ino()),
                "changed lock identity",
            )?;
            if record.broken_import || record.typed_session {
                require(
                    lean_toolchain::shared_writer::previous_writer_stopped(&f)?,
                    "prior writer remains active",
                )?;
            }
            held.push(f);
        }
        drop(admission);
        Ok(Self { record, _locks: held })
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
                return require(address.is_null(), "interop initializer already globally visible");
            }
            require(!address.is_null(), "interop initializer missing from global scope")?;
            let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
            require(
                libc::dladdr(address, info.as_mut_ptr()) != 0,
                "interop initializer has no loaded image",
            )?;
            let info = info.assume_init();
            require(!info.dli_fname.is_null(), "missing loaded interop image path")?;
            let actual = Path::new(std::ffi::OsStr::from_bytes(CStr::from_ptr(info.dli_fname).to_bytes()));
            require(
                actual.canonicalize()? == self.record.interop_image,
                "wrong mapped global interop initializer",
            )?;
        }
        Ok(())
    }
    pub fn verify(&self) -> R<()> {
        let d = &self.record;
        let unexpected_loader_selection = if cfg!(target_os = "macos") {
            std::env::vars_os().any(|(key, _)| key.as_bytes().starts_with(b"DYLD_"))
        } else {
            std::env::vars_os().any(|(key, _)| key.as_bytes().starts_with(b"LD_"))
                || std::env::var_os("GLIBC_TUNABLES").is_some()
        };
        require(
            std::env::var_os("LEAN_SYSROOT").as_deref() == Some(d.sysroot.as_os_str())
                && std::env::var_os("LEAN_PATH").is_none()
                && !unexpected_loader_selection,
            "unexpected runtime selection",
        )?;
        let libs = d.sysroot.join("lib/lean");
        let mut found = false;
        let (lean, init, lake) = runtime_image_names();
        for mapped in mapped_images()? {
            let path = mapped.path;
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let is_runtime = name.starts_with("libleanshared") || name == init || name == lake;
            let is_capability = name.contains("LeanRsInterop")
                || name.contains("LeanRsHostShims")
                || name.contains("LeanRsFixture")
                || name.contains("lean__rs__interop__consumer");
            if !is_runtime && !is_capability {
                continue;
            }
            #[cfg(target_os = "linux")]
            {
                let metadata = path.metadata()?;
                if let (Some((major, minor)), Some(inode)) = (mapped.device, mapped.inode) {
                    require(
                        rustix::fs::major(metadata.dev()) == major
                            && rustix::fs::minor(metadata.dev()) == minor
                            && metadata.ino() == inode,
                        "mapped image path identity changed",
                    )?;
                }
            }
            if is_runtime {
                require(
                    path.canonicalize()? == libs.join(name.as_ref()),
                    "unexpected mapped Lean runtime",
                )?;
                found |= name == lean;
            }
            if is_capability {
                return Err(err("preloaded application capability"));
            }
        }
        require(found, "selected libleanshared absent")?;
        require(
            d.broken_import || fs::read_to_string(&d.consumer_manifest)? == d.consumer_json,
            "consumer manifest or initializer differs",
        )?;
        require(
            d.shim_manifest.canonicalize()? == d.shim_manifest && fs::read_to_string(&d.shim_manifest)? == d.shim_json,
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
        if !d.broken_import {
            d.check_file(&d.consumer_image)?;
        }
        d.check_file(&d.host_image)?;
        for path in d.files.keys() {
            d.check_file(path)?;
        }
        for path in d.content_files.keys() {
            d.check_content_file(path)?;
        }
        for dir in &d.directories {
            require(dir.canonicalize()? == *dir && dir.is_dir(), "import directory changed")?;
            for child in fs::read_dir(dir)? {
                let p = child?.path();
                require(
                    d.files.contains_key(&p) || d.directories.contains(&p),
                    "new entry in selected import tree",
                )?;
            }
        }
        self.check_interop_visibility(false)?;
        if !d.broken_import {
            audit("verified-deployment");
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_mapping_tests {
    use super::*;

    #[test]
    fn proc_maps_preserves_device_inode_and_literal_path_bytes() {
        let maps = b"1000-2000 r-xp 00000000 08:01 42 /opt/lean/lib/lean/libLake shared\\377.so\n";
        let images = parse_linux_maps(maps).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].path, Path::new("/opt/lean/lib/lean/libLake shared\\377.so"));
        assert_eq!(images[0].device, Some((8, 1)));
        assert_eq!(images[0].inode, Some(42));
    }

    #[test]
    fn proc_maps_rejects_deleted_or_conflicting_identity() {
        let deleted = b"1000-2000 r-xp 0 08:01 42 /opt/lib.so (deleted)\n";
        assert!(parse_linux_maps(deleted).is_err());
        let conflicting = b"1000-2000 r-xp 0 08:01 42 /opt/lib.so\n2000-3000 r--p 0 08:01 43 /opt/lib.so\n";
        assert!(parse_linux_maps(conflicting).is_err());
    }

    #[test]
    fn proc_maps_refuses_ambiguous_newline_escape() {
        let maps = b"1000-2000 r-xp 0 08:01 42 /opt/new\\012line.so\n";
        assert!(parse_linux_maps(maps).is_err());
    }
}
