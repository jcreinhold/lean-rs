//! Questions lean-rs asks Lake about a workspace.
//!
//! Lake owns a workspace's layout: which packages it holds, where each one
//! builds, and what a library's shared file is called. [`LakeWorkspace`] runs
//! Lake and returns its answers, so lean-rs reads none of Lake's files and
//! builds no output path itself.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::diagnostics::LinkDiagnostics;

/// Exit code of a `--no-build` Lake command that found something to build.
const NO_BUILD_EXIT_CODE: i32 = 3;

/// Lake's own `.olean`, which marks the directory Lake adds to `LEAN_PATH`.
const LAKE_OLEAN: &str = "Lake.olean";

/// A Lake workspace and the settings lean-rs passes to each Lake command run in it.
///
/// Each query runs one Lake command in the workspace root. Lake loads the
/// workspace before it does anything else, so even a command that builds
/// nothing may write Lake's own files, such as its configuration cache and
/// `lake-manifest.json`, and may fetch missing dependencies.
#[derive(Clone, Debug)]
pub struct LakeWorkspace {
    root: PathBuf,
    lean_sysroot: Option<PathBuf>,
    lean_num_threads: Option<u32>,
    config_options: Vec<(String, String)>,
    reconfigure: bool,
}

impl LakeWorkspace {
    /// Name the workspace whose root directory holds `lakefile.lean` or `lakefile.toml`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lean_sysroot: None,
            lean_num_threads: None,
            config_options: Vec::new(),
            reconfigure: false,
        }
    }

    /// Run `<sysroot>/bin/lake` with `LEAN_SYSROOT` set to `sysroot`.
    ///
    /// Without a sysroot the command runs the first `lake` on `PATH`, which
    /// elan may resolve to the toolchain the workspace pins.
    #[must_use]
    pub fn lean_sysroot(mut self, sysroot: impl Into<PathBuf>) -> Self {
        self.lean_sysroot = Some(sysroot.into());
        self
    }

    /// Cap Lake's parallelism by setting `LEAN_NUM_THREADS` on the command.
    ///
    /// Lake has no `--jobs` flag; it takes both its job count and each `lean`
    /// process's thread count from `LEAN_NUM_THREADS`. Unset, Lake starts one
    /// `lean` per core, and each can use gigabytes of memory.
    #[must_use]
    pub fn lean_num_threads(mut self, threads: u32) -> Self {
        self.lean_num_threads = Some(threads);
        self
    }

    /// Set a lakefile option, passed to Lake as `-Kname=value`.
    ///
    /// Lake reads the option only when it elaborates the root lakefile. It
    /// reuses a cached configuration without checking which options built it,
    /// so add [`Self::reconfigure`] when an earlier run may have used others.
    #[must_use]
    pub(crate) fn config_option(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.config_options.push((name.into(), value.into()));
        self
    }

    /// Make Lake elaborate every lakefile in the workspace again, passed as `-R`.
    #[must_use]
    pub(crate) fn reconfigure(mut self) -> Self {
        self.reconfigure = true;
        self
    }

    /// The workspace root, as given.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.olean` directories Lake puts on `LEAN_PATH`, dependencies first and the root last.
    ///
    /// This runs `lake env` and builds nothing. Lake ends the list with its
    /// own library directory, the one holding `Lake.olean`; this checks that
    /// entry and drops it. A directory may not exist until Lake builds its
    /// package.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeConfigMissing`] without running Lake
    /// when the root has no lakefile, [`LinkDiagnostics::LakeUnavailable`] or
    /// [`LinkDiagnostics::LakeFailed`] when Lake does not run or fails, and
    /// [`LinkDiagnostics::LakeAnswerInvalid`] when its output has no usable
    /// `LEAN_PATH` line.
    pub fn lean_path(&self) -> Result<Vec<PathBuf>, LinkDiagnostics> {
        self.lean_path_with(&mut RealLakeRunner)
    }

    /// Build `target`'s shared library if Lake finds it out of date, and return its path.
    ///
    /// This runs `lake query <target>:shared`, which builds the library and
    /// the libraries it imports, then prints the file's path. Lake names the
    /// file, from the library's `libName` when the lakefile sets one.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeTargetMissing`] when Lake reports no
    /// library named `target`, the errors of [`Self::lean_path`] for a missing
    /// lakefile or a failed run, and [`LinkDiagnostics::LakeAnswerInvalid`]
    /// when Lake prints anything but one absolute path to a file.
    pub fn shared_library(&self, target: &str) -> Result<PathBuf, LinkDiagnostics> {
        self.shared_library_with(&mut RealLakeRunner, target)
    }

    /// Return `target`'s shared library if Lake reports it up to date, without building it.
    ///
    /// This runs `lake --no-build query <target>:shared`. Lake exits with code
    /// 3 when the library, or anything it needs, would have to be built; that
    /// is `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::shared_library`].
    pub fn built_shared_library(&self, target: &str) -> Result<Option<PathBuf>, LinkDiagnostics> {
        self.built_shared_library_with(&mut RealLakeRunner, target)
    }

    pub(crate) fn lean_path_with(&self, runner: &mut impl LakeRunner) -> Result<Vec<PathBuf>, LinkDiagnostics> {
        let (stdout, command) = self.run(runner, LakeCall::Env)?;
        let stdout = stdout.ok_or_else(|| self.answer_invalid(&command, "Lake asked to build".to_owned()))?;
        parse_lean_path(&stdout).map_err(|reason| self.answer_invalid(&command, reason))
    }

    pub(crate) fn shared_library_with(
        &self,
        runner: &mut impl LakeRunner,
        target: &str,
    ) -> Result<PathBuf, LinkDiagnostics> {
        let (stdout, command) = self.run(runner, LakeCall::Query { target, build: true })?;
        let stdout = stdout.ok_or_else(|| self.answer_invalid(&command, "Lake asked to build".to_owned()))?;
        parse_library(&stdout).map_err(|reason| self.answer_invalid(&command, reason))
    }

    pub(crate) fn built_shared_library_with(
        &self,
        runner: &mut impl LakeRunner,
        target: &str,
    ) -> Result<Option<PathBuf>, LinkDiagnostics> {
        let (stdout, command) = self.run(runner, LakeCall::Query { target, build: false })?;
        stdout
            .map(|stdout| parse_library(&stdout).map_err(|reason| self.answer_invalid(&command, reason)))
            .transpose()
    }

    /// Run one Lake command. `Ok((None, _))` means a `--no-build` query found
    /// something to build.
    fn run(
        &self,
        runner: &mut impl LakeRunner,
        call: LakeCall<'_>,
    ) -> Result<(Option<Vec<u8>>, String), LinkDiagnostics> {
        if !["lakefile.lean", "lakefile.toml"]
            .iter()
            .any(|name| self.root.join(name).is_file())
        {
            return Err(LinkDiagnostics::LakeConfigMissing {
                project_root: self.root.clone(),
            });
        }
        let mut args = Vec::new();
        if self.reconfigure {
            args.push("-R".to_owned());
        }
        for (name, value) in &self.config_options {
            args.push(format!("-K{name}={value}"));
        }
        match call {
            LakeCall::Env => args.push("env".to_owned()),
            LakeCall::Query { target, build } => {
                if !build {
                    args.push("--no-build".to_owned());
                }
                args.push("query".to_owned());
                args.push(format!("{target}:shared"));
            }
        }
        let line = std::iter::once("lake")
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = if let Some(sysroot) = &self.lean_sysroot {
            let mut command = Command::new(sysroot.join("bin").join("lake"));
            command.env("LEAN_SYSROOT", sysroot);
            command
        } else {
            Command::new("lake")
        };
        if let Some(threads) = self.lean_num_threads {
            command.env("LEAN_NUM_THREADS", threads.to_string());
        }
        // Lake appends an inherited `LEAN_PATH` to the one it reports and
        // passes to `lean`; remove it so the answer is the workspace's alone.
        command.env_remove("LEAN_PATH").args(&args).current_dir(&self.root);
        let output = runner
            .output(&mut command)
            .map_err(|error| LinkDiagnostics::LakeUnavailable {
                project_root: self.root.clone(),
                command: line.clone(),
                detail: error.to_string(),
            })?;
        if output.status.success() {
            return Ok((Some(output.stdout), line));
        }
        if let LakeCall::Query { target, build } = call {
            if !build && output.status.code() == Some(NO_BUILD_EXIT_CODE) {
                return Ok((None, line));
            }
            if reports_missing_library(&output.stderr) {
                return Err(LinkDiagnostics::LakeTargetMissing {
                    project_root: self.root.clone(),
                    target_name: target.to_owned(),
                });
            }
        }
        Err(LinkDiagnostics::LakeFailed {
            project_root: self.root.clone(),
            command: line,
            status: output.status.to_string(),
            detail: command_detail(&output.stdout, &output.stderr),
        })
    }

    fn answer_invalid(&self, command: &str, reason: String) -> LinkDiagnostics {
        LinkDiagnostics::LakeAnswerInvalid {
            project_root: self.root.clone(),
            command: command.to_owned(),
            reason,
        }
    }
}

#[derive(Clone, Copy)]
enum LakeCall<'a> {
    Env,
    Query { target: &'a str, build: bool },
}

/// Runs a prepared Lake command; tests substitute a fake Lake.
pub(crate) trait LakeRunner {
    fn output(&mut self, command: &mut Command) -> io::Result<Output>;
}

/// Runs the real `lake` executable.
pub(crate) struct RealLakeRunner;

impl LakeRunner for RealLakeRunner {
    fn output(&mut self, command: &mut Command) -> io::Result<Output> {
        command.output()
    }
}

/// Lake's messages for a target spec that names no library: an unknown name,
/// a package, module, executable or custom target where a library was asked
/// for, and an unknown package in a `package/target` spec.
fn reports_missing_library(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.lines().any(|line| {
        [
            "unknown target `",
            "has no target '",
            "unknown package `",
            "invalid facet `shared`",
        ]
        .iter()
        .any(|message| line.contains(message))
            || (line.contains("unknown ") && line.contains(" facet `shared`"))
    })
}

fn parse_lean_path(stdout: &[u8]) -> Result<Vec<PathBuf>, String> {
    let stdout = std::str::from_utf8(stdout).map_err(|error| format!("output is not UTF-8 ({error})"))?;
    let value = stdout
        .lines()
        .find_map(|line| line.strip_prefix("LEAN_PATH="))
        .ok_or_else(|| "no LEAN_PATH line".to_owned())?;
    let mut roots = std::env::split_paths(value).collect::<Vec<_>>();
    let lake_dir = roots.pop().ok_or_else(|| "LEAN_PATH is empty".to_owned())?;
    if !lake_dir.join(LAKE_OLEAN).is_file() {
        return Err(format!(
            "the last LEAN_PATH entry {} does not hold {LAKE_OLEAN}",
            lake_dir.display()
        ));
    }
    if roots.is_empty() {
        return Err("LEAN_PATH names no package directory".to_owned());
    }
    if let Some(relative) = roots.iter().find(|root| !root.is_absolute()) {
        return Err(format!("LEAN_PATH entry {} is not absolute", relative.display()));
    }
    Ok(roots)
}

fn parse_library(stdout: &[u8]) -> Result<PathBuf, String> {
    let stdout = std::str::from_utf8(stdout).map_err(|error| format!("output is not UTF-8 ({error})"))?;
    let mut lines = stdout.lines().filter(|line| !line.is_empty());
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return Err("expected exactly one line naming the library".to_owned());
    };
    let path = PathBuf::from(line);
    if !path.is_absolute() {
        return Err(format!("library path {} is not absolute", path.display()));
    }
    if !path.is_file() {
        return Err(format!("library path {} is not a file", path.display()));
    }
    Ok(path)
}

/// A failed command's stderr and stdout on one line of at most 1024 characters.
fn command_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut raw = String::from_utf8_lossy(stderr).into_owned();
    if !stdout.is_empty() {
        if !raw.is_empty() {
            raw.push_str(" | ");
        }
        raw.push_str(&String::from_utf8_lossy(stdout));
    }
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "no output".to_owned()
    } else if collapsed.chars().count() > 1024 {
        let mut bounded = collapsed.chars().take(1024).collect::<String>();
        bounded.push_str("...");
        bounded
    } else {
        collapsed
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use std::fs;
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{LakeRunner, LakeWorkspace};
    use crate::diagnostics::LinkDiagnostics;

    static NONCE: AtomicU64 = AtomicU64::new(0);

    /// One recorded Lake command: program, arguments, environment changes and directory.
    #[derive(Debug)]
    pub(crate) struct LakeCallRecord {
        pub(crate) program: PathBuf,
        pub(crate) args: Vec<String>,
        pub(crate) envs: Vec<(String, Option<String>)>,
        pub(crate) dir: Option<PathBuf>,
    }

    /// A fake Lake that answers each command from a closure and records it.
    pub(crate) struct FakeLake<F> {
        pub(crate) calls: Vec<LakeCallRecord>,
        answer: F,
    }

    impl<F: FnMut(&[String]) -> Output> FakeLake<F> {
        pub(crate) fn new(answer: F) -> Self {
            Self {
                calls: Vec::new(),
                answer,
            }
        }
    }

    impl<F: FnMut(&[String]) -> Output> LakeRunner for FakeLake<F> {
        fn output(&mut self, command: &mut Command) -> std::io::Result<Output> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            self.calls.push(LakeCallRecord {
                program: PathBuf::from(command.get_program()),
                args: args.clone(),
                envs: command
                    .get_envs()
                    .map(|(name, value)| {
                        (
                            name.to_string_lossy().into_owned(),
                            value.map(|value| value.to_string_lossy().into_owned()),
                        )
                    })
                    .collect(),
                dir: command.get_current_dir().map(Path::to_path_buf),
            });
            Ok((self.answer)(&args))
        }
    }

    /// A Lake answer with exit `code` and the given output.
    pub(crate) fn answer(code: i32, stdout: &str, stderr: &str) -> Output {
        // A wait status holds the exit code in its second byte.
        let status = match code {
            0 => 0,
            1 => 0x100,
            3 => 0x300,
            _ => 0x7f00,
        };
        Output {
            status: ExitStatus::from_raw(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// A fresh directory under the system temporary directory.
    pub(crate) fn temp_dir(name: &str) -> Result<PathBuf, String> {
        let dir = std::env::temp_dir().join(format!(
            "lean-toolchain-lake-{name}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        dir.canonicalize().map_err(|error| error.to_string())
    }

    /// A workspace root with a lakefile, and a directory holding `Lake.olean`.
    pub(crate) fn fake_project(name: &str) -> Result<(PathBuf, PathBuf), String> {
        let dir = temp_dir(name)?;
        let root = dir.join("project");
        let lake = dir.join("toolchain/lib/lean");
        for path in [&root, &lake] {
            fs::create_dir_all(path).map_err(|error| error.to_string())?;
        }
        fs::write(root.join("lakefile.lean"), "package demo\n").map_err(|error| error.to_string())?;
        fs::write(lake.join("Lake.olean"), "").map_err(|error| error.to_string())?;
        Ok((root, lake))
    }

    fn lean_path_line(entries: &[&Path]) -> Result<String, String> {
        let joined = std::env::join_paths(entries).map_err(|error| error.to_string())?;
        Ok(format!(
            "LEAN_SYSROOT=/x\nLEAN_PATH={}\nPATH=/bin\n",
            joined.to_string_lossy()
        ))
    }

    #[test]
    fn lean_path_keeps_lakes_order_and_drops_lakes_own_directory() -> Result<(), String> {
        let (root, lake) = fake_project("env")?;
        let dependency = root.join("dep/out/lib/lean");
        let own = root.join("out/lib/lean");
        let stdout = lean_path_line(&[&dependency, &own, &lake])?;
        let mut fake = FakeLake::new(|_: &[String]| answer(0, &stdout, ""));
        let roots = LakeWorkspace::new(&root)
            .lean_path_with(&mut fake)
            .map_err(|error| error.to_string())?;
        assert_eq!(roots, vec![dependency, own]);
        let call = fake.calls.first().ok_or("no Lake call")?;
        assert_eq!(call.program, PathBuf::from("lake"));
        assert_eq!(call.args, ["env"]);
        assert_eq!(call.dir.as_deref(), Some(root.as_path()));
        assert!(call.envs.contains(&("LEAN_PATH".to_owned(), None)));
        Ok(())
    }

    #[test]
    fn lean_path_refuses_an_answer_lean_toolchain_cannot_check() -> Result<(), String> {
        let (root, lake) = fake_project("env-invalid")?;
        let own = root.join("out/lib/lean");
        let cases = [
            "PATH=/bin\n".to_owned(),
            lean_path_line(&[&own])?,
            lean_path_line(&[&lake])?,
            lean_path_line(&[Path::new("relative/lib/lean"), &lake])?,
        ];
        for stdout in cases {
            let mut fake = FakeLake::new(|_: &[String]| answer(0, &stdout, ""));
            let result = LakeWorkspace::new(&root).lean_path_with(&mut fake);
            assert!(
                matches!(result, Err(LinkDiagnostics::LakeAnswerInvalid { .. })),
                "{stdout:?} gave {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn options_come_before_the_command() -> Result<(), String> {
        let (root, _) = fake_project("options")?;
        let mut fake = FakeLake::new(|_: &[String]| answer(3, "", ""));
        let sysroot = root.join("sysroot");
        let found = LakeWorkspace::new(&root)
            .lean_sysroot(&sysroot)
            .lean_num_threads(2)
            .config_option("flag", "true")
            .reconfigure()
            .built_shared_library_with(&mut fake, "Demo")
            .map_err(|error| error.to_string())?;
        assert_eq!(found, None);
        let call = fake.calls.first().ok_or("no Lake call")?;
        assert_eq!(call.program, sysroot.join("bin/lake"));
        assert_eq!(call.args, ["-R", "-Kflag=true", "--no-build", "query", "Demo:shared"]);
        for env in [
            ("LEAN_SYSROOT".to_owned(), Some(sysroot.display().to_string())),
            ("LEAN_NUM_THREADS".to_owned(), Some("2".to_owned())),
        ] {
            assert!(call.envs.contains(&env), "missing {env:?} in {:?}", call.envs);
        }
        Ok(())
    }

    #[test]
    fn a_query_returns_the_one_file_lake_prints() -> Result<(), String> {
        let (root, _) = fake_project("query")?;
        let library = root.join("out/lib/libdemo_Demo.dylib");
        fs::create_dir_all(root.join("out/lib")).map_err(|error| error.to_string())?;
        fs::write(&library, "").map_err(|error| error.to_string())?;
        let stdout = format!("{}\n", library.display());
        let mut fake = FakeLake::new(|_: &[String]| answer(0, &stdout, "Build completed"));
        let workspace = LakeWorkspace::new(&root);
        assert_eq!(
            workspace
                .shared_library_with(&mut fake, "Demo")
                .map_err(|error| error.to_string())?,
            library
        );
        assert_eq!(
            workspace
                .built_shared_library_with(&mut fake, "Demo")
                .map_err(|error| error.to_string())?,
            Some(library.clone())
        );
        let args = fake.calls.iter().map(|call| call.args.clone()).collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                vec!["query".to_owned(), "Demo:shared".to_owned()],
                vec!["--no-build".to_owned(), "query".to_owned(), "Demo:shared".to_owned()],
            ]
        );
        let bad = [
            String::new(),
            "relative.dylib\n".to_owned(),
            format!("{}\n", root.join("missing.dylib").display()),
            format!("{0}\n{0}\n", library.display()),
        ];
        for stdout in bad {
            let mut fake = FakeLake::new(|_: &[String]| answer(0, &stdout, ""));
            let result = workspace.shared_library_with(&mut fake, "Demo");
            assert!(
                matches!(result, Err(LinkDiagnostics::LakeAnswerInvalid { .. })),
                "{stdout:?} gave {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn lakes_messages_for_a_missing_library_name_it() -> Result<(), String> {
        let (root, _) = fake_project("missing")?;
        for stderr in [
            "error: unknown target `Demo`",
            "error: unknown package facet `shared`",
            "error: unknown module facet `shared`",
            "error: invalid facet `shared`; target Demo has no facets",
            "error: package 'demo' has no target 'Demo'",
            "error: unknown package `other`",
        ] {
            let mut fake = FakeLake::new(|_: &[String]| answer(1, "", stderr));
            let result = LakeWorkspace::new(&root).shared_library_with(&mut fake, "Demo");
            assert!(
                matches!(&result, Err(LinkDiagnostics::LakeTargetMissing { target_name, .. }) if target_name == "Demo"),
                "{stderr:?} gave {result:?}"
            );
        }
        let mut fake = FakeLake::new(|_: &[String]| answer(1, "", "error: Demo.lean:1:0: unknown identifier"));
        let result = LakeWorkspace::new(&root).shared_library_with(&mut fake, "Demo");
        let Err(LinkDiagnostics::LakeFailed { command, detail, .. }) = result else {
            return Err(format!("a build error gave {result:?}"));
        };
        assert_eq!(command, "lake query Demo:shared");
        assert!(detail.contains("unknown identifier"), "{detail}");
        Ok(())
    }

    #[test]
    fn a_root_without_a_lakefile_runs_no_lake() -> Result<(), String> {
        let root = temp_dir("no-lakefile")?;
        let mut fake = FakeLake::new(|_: &[String]| answer(0, "", ""));
        let result = LakeWorkspace::new(&root).lean_path_with(&mut fake);
        assert!(
            matches!(result, Err(LinkDiagnostics::LakeConfigMissing { .. })),
            "{result:?}"
        );
        assert!(fake.calls.is_empty());
        Ok(())
    }

    #[test]
    fn failure_detail_is_one_bounded_line() {
        let detail = super::command_detail(&[b'x'; 4096], b"error:\n  line");
        assert!(detail.starts_with("error: line | x"), "{detail}");
        assert_eq!(detail.chars().count(), 1027);
        assert!(detail.ends_with("..."));
        assert_eq!(super::command_detail(b"", b""), "no output");
    }
}
