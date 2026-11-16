//! Actual-worker custody over the canonical shared artifact domains.
//!
//! Only the worker makes transferred descriptors inheritable. Dropping this
//! scope closes aliases; it never unlocks a descriptor inherited by body work.
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::parent_watchdog::GuardianHandle;
use lean_rs_worker_protocol::types::LeanWorkerArtifactReadEpoch;
use lean_toolchain::shared_reader::SharedReader;

pub(crate) struct ArtifactReadScope {
    _readers: Vec<SharedReader>,
    epochs: Vec<LeanWorkerArtifactReadEpoch>,
}

impl ArtifactReadScope {
    pub(crate) fn acquire(
        helper: &Path,
        roots: &[PathBuf],
        timeout: Duration,
        guardian: Option<&mut GuardianHandle>,
    ) -> io::Result<Self> {
        let guardian =
            guardian.ok_or_else(|| io::Error::other("actual native reads require independent guardian custody"))?;
        let started = Instant::now();
        if roots.len() > lean_toolchain::native_guardian::MAX_READ_DOMAINS {
            return Err(io::Error::other("host artifact domain union exceeds limit"));
        }
        let mut keyed = roots
            .iter()
            .map(|root| {
                let root = root.canonicalize()?;
                Ok((lean_toolchain::shared_writer::project_lock_path(&root)?, root))
            })
            .collect::<io::Result<Vec<_>>>()?;
        // Writers take EX in stable lock-path order. Every multi-domain SH
        // acquisition must use that same order to avoid a reader/writer cycle.
        keyed.sort();
        keyed.dedup();
        let domains: Vec<_> = keyed.into_iter().map(|(_, root)| root).collect();
        if domains.is_empty() {
            return Err(io::Error::other("host has no shared artifact domains"));
        }
        let mut readers = Vec::with_capacity(domains.len());
        let mut epochs = Vec::with_capacity(domains.len());
        for root in &domains {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "artifact read admission expired",
                ));
            }
            let reader = SharedReader::acquire(helper, root, remaining)?;
            // This code runs inside the controlled native worker, before its
            // body can spawn descendants, never inside the multithreaded broker.
            reader.inherit_by_body_children()?;
            epochs.push(LeanWorkerArtifactReadEpoch {
                project_root: root.to_string_lossy().into_owned(),
                epoch: reader.epoch().to_owned(),
            });
            readers.push(reader);
        }
        let loans = readers
            .iter()
            .zip(&domains)
            .map(|(reader, root)| reader.guardian_loan(root))
            .collect::<io::Result<Vec<_>>>()?;
        guardian.retain_reads(&loans, timeout.saturating_sub(started.elapsed()))?;
        drop(loans);
        Ok(Self {
            _readers: readers,
            epochs,
        })
    }

    pub(crate) fn epochs(&self) -> &[LeanWorkerArtifactReadEpoch] {
        &self.epochs
    }
}
