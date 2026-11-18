//! Private input checks for exclusively provisioned fixture processes.
use lean_toolchain::shared_reader::SharedReader;
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};
pub(super) fn check_digest(path: &Path, approved: Option<&str>) -> io::Result<()> {
    let expected =
        approved.ok_or_else(|| io::Error::other("compile this profiling target with its reviewed fixture digests"))?;
    let hash = Sha256::digest(fs::read(path)?);
    use std::fmt::Write as _;
    let mut actual = String::with_capacity(64);
    for byte in hash {
        write!(actual, "{byte:02x}").map_err(io::Error::other)?;
    }
    if actual != expected {
        return Err(io::Error::other(format!("profiling input changed: {}", path.display())));
    }
    Ok(())
}

pub(super) fn hold(roots: &[PathBuf]) -> io::Result<Vec<SharedReader>> {
    let prefix = Path::new(lean_toolchain::LEAN_HEADER_PATH)
        .ancestors()
        .nth(3)
        .ok_or_else(|| io::Error::other("missing build prefix"))?
        .canonicalize()?;
    if std::env::var_os("LEAN_SYSROOT").as_deref() != Some(prefix.as_os_str())
        || std::env::var_os("LEAN_PATH").is_some()
    {
        return Err(io::Error::other(
            "profiling launcher must pin its build sysroot and clear LEAN_PATH",
        ));
    }
    let helper = Path::new(
        option_env!("LEAN_RS_PROFILE_READER").ok_or_else(|| io::Error::other("missing reviewed reader helper"))?,
    );
    if helper.canonicalize()? != helper {
        return Err(io::Error::other("noncanonical helper"));
    }
    check_digest(helper, option_env!("LEAN_RS_PROFILE_READER_SHA256"))?;
    SharedReader::acquire_union(helper, roots, Duration::from_secs(30), &|| false)
}
