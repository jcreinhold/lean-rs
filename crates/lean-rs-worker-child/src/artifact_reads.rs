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
        allow_wait: bool,
        guardian: Option<&mut GuardianHandle>,
        ipc_root: Option<&Path>,
        planned: bool,
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
        if planned {
            guardian.plan_reads(&domains, timeout.saturating_sub(started.elapsed()))?;
        }
        // This child has no request-local cancellation flag: the existing
        // parent lifeline and supervisor cancel its owned process group. Both
        // collector modes retain this operation's one original time budget.
        let remaining = timeout.saturating_sub(started.elapsed());
        let readers = if allow_wait {
            match ipc_root {
                Some(root) => SharedReader::acquire_union_with_ipc_root(helper, &domains, remaining, &|| false, root)?,
                None => SharedReader::acquire_union(helper, &domains, remaining, &|| false)?,
            }
        } else {
            // The guardian still owns the previous call's SH descriptions for
            // this generation. Waiting here would deadlock a demanded writer
            // against this already-admitted call. Typed contention is returned
            // before any inheritance or new guardian transfer, so the parent
            // can cycle the old generation before one fresh pre-body attempt.
            match ipc_root {
                Some(root) => {
                    SharedReader::try_acquire_union_with_ipc_root(helper, &domains, remaining, &|| false, root)?
                }
                None => SharedReader::try_acquire_union(helper, &domains, remaining, &|| false)?,
            }
        };
        let mut epochs = Vec::with_capacity(domains.len());
        for (reader, root) in readers.iter().zip(&domains) {
            // Acquisition and all contention retries finish before the first
            // inheritable descriptor or irreversible guardian retain/ACK.
            reader.inherit_by_body_children()?;
            epochs.push(LeanWorkerArtifactReadEpoch {
                project_root: root.to_string_lossy().into_owned(),
                epoch: reader.epoch().to_owned(),
            });
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
