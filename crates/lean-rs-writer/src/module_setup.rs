//! Command-specific required output, staged under the already admitted scratch descriptor.
use super::{File, Path, Read as _, Value, Write as _, field, invalid, io, json, scratch::Scratch, storage::Request};
use rustix::fs::{Mode, OFlags, open};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;

const MARKER: &str = "LEAN_HOST_MCP_CAPTURE_MODULE_SETUP";
const LIMIT: usize = 16 * 1024 * 1024;

pub(super) struct Destination<'a> {
    scratch: &'a Scratch<'a>,
    pub(super) limit: usize,
}

impl Destination<'_> {
    pub(super) fn publish(&self, bytes: &[u8]) -> io::Result<String> {
        self.scratch.create_file("module-setup.json")?.write_all(bytes)?;
        self.scratch.verified_path()?;
        use std::fmt::Write as _;
        let mut digest = String::with_capacity(64);
        for byte in Sha256::digest(bytes) {
            write!(digest, "{byte:02x}").map_err(io::Error::other)?;
        }
        Ok(digest)
    }
}

fn pinned_lake(root: &Path) -> io::Result<PathBuf> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let pin_file = File::from(open(root.join("lean-toolchain"), flags, Mode::empty())?);
    if !pin_file.metadata()?.is_file() {
        return Err(invalid("expected a regular lean-toolchain file"));
    }
    let mut pin = String::new();
    pin_file.take(129).read_to_string(&mut pin)?;
    if pin.len() > 128 {
        return Err(invalid("oversized lean-toolchain pin"));
    }
    let pin = pin.trim();
    let id = pin.strip_prefix("leanprover/lean4:").unwrap_or(pin);
    if lean_toolchain::supported_for(id.strip_prefix('v').unwrap_or(id)).is_none() {
        return Err(invalid("module setup requires a supported project toolchain"));
    }
    let home = std::env::var_os("ELAN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|p| p.join(".elan")))
        .filter(|p| p.is_absolute())
        .ok_or_else(|| invalid("absolute Elan home required"))?;
    Ok(home
        .join("toolchains")
        .join(format!("leanprover--lean4---{id}"))
        .canonicalize()?
        .join("bin/lake"))
}

pub(super) fn select<'a>(
    message: &Value,
    root: &Path,
    scratch: Option<&'a Scratch<'a>>,
    request: Option<&Request>,
) -> io::Result<Option<Destination<'a>>> {
    let env = field(message, "env");
    if env.get(MARKER).is_none() {
        return Ok(None);
    }
    let refused = || invalid("marked stage requires charged scratch and the selected Lake no-build setup-file command");
    let scratch = scratch.ok_or_else(refused)?;
    let request = request.ok_or_else(refused)?;
    let assets = scratch.verified_path()?;
    let args = field(message, "command").as_array().ok_or_else(refused)?;
    let [program, no_build, no_cache, setup_file, file] = args.as_slice() else {
        return Err(refused());
    };
    if env != &json!({(MARKER): "1", "LAKE_CACHE_DIR": assets, "LAKE_ARTIFACT_CACHE": "false"})
        || field(message, "cwd").as_str() != root.to_str()
        || no_build != "--no-build"
        || no_cache != "--no-cache"
        || setup_file != "setup-file"
    {
        return Err(refused());
    }
    let lake = pinned_lake(root)?;
    let file = Path::new(file.as_str().ok_or_else(refused)?);
    if program.as_str() != lake.to_str()
        || !lake.is_file()
        || lake.canonicalize()? != lake
        || !file.starts_with(root)
        || file.extension().is_none_or(|s| s != "lean")
        || !file.is_file()
        || file.canonicalize()? != file
    {
        return Err(refused());
    }
    Ok(Some(Destination {
        scratch,
        limit: LIMIT.min(usize::try_from(request.budget_bytes).unwrap_or(usize::MAX)),
    }))
}
