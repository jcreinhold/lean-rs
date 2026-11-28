//! Questions lean-rs asks Lake about a workspace.
//!
//! Lake owns a workspace's layout: which packages it holds, where each one
//! builds, and what a library's shared file is called. [`LakeWorkspace`] asks
//! Lake and reads its answers, so lean-rs reads none of Lake's files and
//! builds no output path itself.
//!
//! Each question comes as a pair: a method that builds the Lake command and a
//! method that reads its output. A caller that runs processes under its own
//! time limit, process group or turn runs the command itself and hands the
//! output back. The methods named for the answer run the command directly,
//! with no time limit.

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

    /// The `lake env` command whose output [`Self::read_lean_path`] reads.
    ///
    /// The command builds nothing. It sets its program, arguments, working
    /// directory and environment, and removes an inherited `LEAN_PATH`, which
    /// Lake would add to its answer. The caller sets stdio and runs it.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeConfigMissing`] when the root has no
    /// lakefile.
    pub fn lean_path_command(&self) -> Result<Command, LinkDiagnostics> {
        self.command(LakeCall::Env)
    }

    /// Read the `.olean` directories, dependencies first and the root last,
    /// from the output of [`Self::lean_path_command`].
    ///
    /// Lake ends its `LEAN_PATH` with its own library directory, the one
    /// holding `Lake.olean`; this checks that entry and drops it. A directory
    /// may not exist until Lake builds its package.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeFailed`] when the command failed, and
    /// [`LinkDiagnostics::LakeAnswerInvalid`] when its stdout has no usable
    /// `LEAN_PATH` line.
    pub fn read_lean_path(&self, output: &Output) -> Result<Vec<PathBuf>, LinkDiagnostics> {
        let call = LakeCall::Env;
        parse_lean_path(self.answer(call, output)?).map_err(|reason| self.answer_invalid(call, reason))
    }

    /// Run [`Self::lean_path_command`] with no time limit and read its output.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::lean_path_command`] and
    /// [`Self::read_lean_path`], and [`LinkDiagnostics::LakeUnavailable`]
    /// when Lake does not start.
    pub fn lean_path(&self) -> Result<Vec<PathBuf>, LinkDiagnostics> {
        self.lean_path_with(&mut RealLakeRunner)
    }

    /// The `lake query <target>:shared` command whose output
    /// [`Self::read_shared_library`] reads.
    ///
    /// Lake builds the library and the libraries it imports when they are out
    /// of date, then prints the file's path. Lake names the file, from the
    /// library's `libName` when the lakefile sets one. The command is set up
    /// as [`Self::lean_path_command`] describes.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeConfigMissing`] when the root has no
    /// lakefile.
    pub fn shared_library_command(&self, target: &str) -> Result<Command, LinkDiagnostics> {
        self.command(LakeCall::Query { target, build: true })
    }

    /// Read `target`'s shared library from the output of [`Self::shared_library_command`].
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeTargetMissing`] when Lake reports no
    /// library named `target`, [`LinkDiagnostics::LakeFailed`] when the
    /// command failed otherwise, and [`LinkDiagnostics::LakeAnswerInvalid`]
    /// when its stdout is anything but one absolute path to a file.
    pub fn read_shared_library(&self, target: &str, output: &Output) -> Result<PathBuf, LinkDiagnostics> {
        let call = LakeCall::Query { target, build: true };
        parse_library(self.answer(call, output)?).map_err(|reason| self.answer_invalid(call, reason))
    }

    /// Run [`Self::shared_library_command`] with no time limit and read its output.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::shared_library_command`] and
    /// [`Self::read_shared_library`], and [`LinkDiagnostics::LakeUnavailable`]
    /// when Lake does not start.
    pub fn shared_library(&self, target: &str) -> Result<PathBuf, LinkDiagnostics> {
        self.shared_library_with(&mut RealLakeRunner, target)
    }

    /// The `lake --no-build query <target>:shared` command whose output
    /// [`Self::read_built_shared_library`] reads.
    ///
    /// The command builds nothing. It is set up as
    /// [`Self::lean_path_command`] describes.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics::LakeConfigMissing`] when the root has no
    /// lakefile.
    pub fn built_shared_library_command(&self, target: &str) -> Result<Command, LinkDiagnostics> {
        self.command(LakeCall::Query { target, build: false })
    }

    /// Read `target`'s shared library from the output of
    /// [`Self::built_shared_library_command`], or `None` when Lake would have
    /// to build it.
    ///
    /// Lake exits with code 3 when the library, or anything it needs, is out
    /// of date.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read_shared_library`].
    pub fn read_built_shared_library(&self, target: &str, output: &Output) -> Result<Option<PathBuf>, LinkDiagnostics> {
        if output.status.code() == Some(NO_BUILD_EXIT_CODE) {
            return Ok(None);
        }
        let call = LakeCall::Query { target, build: false };
        parse_library(self.answer(call, output)?)
            .map(Some)
            .map_err(|reason| self.answer_invalid(call, reason))
    }

    /// Run [`Self::built_shared_library_command`] with no time limit and read its output.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::built_shared_library_command`] and
    /// [`Self::read_built_shared_library`], and
    /// [`LinkDiagnostics::LakeUnavailable`] when Lake does not start.
    pub fn built_shared_library(&self, target: &str) -> Result<Option<PathBuf>, LinkDiagnostics> {
        self.built_shared_library_with(&mut RealLakeRunner, target)
    }

    pub(crate) fn lean_path_with(&self, runner: &mut impl LakeRunner) -> Result<Vec<PathBuf>, LinkDiagnostics> {
        self.read_lean_path(&self.run(runner, LakeCall::Env)?)
    }

    pub(crate) fn shared_library_with(
        &self,
        runner: &mut impl LakeRunner,
        target: &str,
    ) -> Result<PathBuf, LinkDiagnostics> {
        self.read_shared_library(target, &self.run(runner, LakeCall::Query { target, build: true })?)
    }

    pub(crate) fn built_shared_library_with(
        &self,
        runner: &mut impl LakeRunner,
        target: &str,
    ) -> Result<Option<PathBuf>, LinkDiagnostics> {
        self.read_built_shared_library(target, &self.run(runner, LakeCall::Query { target, build: false })?)
    }

    /// The Lake command for `call`, after checking that the root has a lakefile.
    fn command(&self, call: LakeCall<'_>) -> Result<Command, LinkDiagnostics> {
        if !["lakefile.lean", "lakefile.toml"]
            .iter()
            .any(|name| self.root.join(name).is_file())
        {
            return Err(LinkDiagnostics::LakeConfigMissing {
                project_root: self.root.clone(),
            });
        }
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
        command
            .env_remove("LEAN_PATH")
            .args(self.args(call))
            .current_dir(&self.root);
        Ok(command)
    }

    fn args(&self, call: LakeCall<'_>) -> Vec<String> {
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
        args
    }

    /// The command for `call` as one line, for diagnostics.
    fn line(&self, call: LakeCall<'_>) -> String {
        std::iter::once("lake".to_owned())
            .chain(self.args(call))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Run the command for `call` through `runner`.
    fn run(&self, runner: &mut impl LakeRunner, call: LakeCall<'_>) -> Result<Output, LinkDiagnostics> {
        let mut command = self.command(call)?;
        runner
            .output(&mut command)
            .map_err(|error| LinkDiagnostics::LakeUnavailable {
                project_root: self.root.clone(),
                command: self.line(call),
                detail: error.to_string(),
            })
    }

    /// Lake's stdout when the command for `call` succeeded.
    fn answer<'o>(&self, call: LakeCall<'_>, output: &'o Output) -> Result<&'o [u8], LinkDiagnostics> {
        if output.status.success() {
            return Ok(&output.stdout);
        }
        if let LakeCall::Query { target, .. } = call
            && reports_missing_library(&output.stderr)
        {
            return Err(LinkDiagnostics::LakeTargetMissing {
                project_root: self.root.clone(),
                target_name: target.to_owned(),
            });
        }
        Err(LinkDiagnostics::LakeFailed {
            project_root: self.root.clone(),
            command: self.line(call),
            status: output.status.to_string(),
            detail: command_detail(&output.stdout, &output.stderr),
        })
    }

    fn answer_invalid(&self, call: LakeCall<'_>, reason: String) -> LinkDiagnostics {
        LinkDiagnostics::LakeAnswerInvalid {
            project_root: self.root.clone(),
            command: self.line(call),
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

    use super::{LakeRunner, LakeWorkspace};
    use crate::diagnostics::LinkDiagnostics;
    use crate::test_dir::{TempDir, temp_dir};

    /// One recorded Lake command: program, arguments, environment changes and directory.
    #[derive(Debug)]
    pub(crate) struct LakeCallRecord {
        pub(crate) program: PathBuf,
        pub(crate) args: Vec<String>,
        pub(crate) envs: Vec<(String, Option<String>)>,
        pub(crate) dir: Option<PathBuf>,
    }

    impl LakeCallRecord {
        fn of(command: &Command) -> Self {
            Self {
                program: PathBuf::from(command.get_program()),
                args: command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
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
            }
        }
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
            let record = LakeCallRecord::of(command);
            let output = (self.answer)(&record.args);
            self.calls.push(record);
            Ok(output)
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

    /// A workspace root with a lakefile, and a directory holding `Lake.olean`.
    ///
    /// Both live in the returned directory; keep it until the test ends.
    pub(crate) fn fake_project(name: &str) -> Result<(TempDir, PathBuf, PathBuf), String> {
        let dir = temp_dir(name)?;
        let root = dir.path().join("project");
        let lake = dir.path().join("toolchain/lib/lean");
        for path in [&root, &lake] {
            fs::create_dir_all(path).map_err(|error| error.to_string())?;
        }
        fs::write(root.join("lakefile.lean"), "package demo\n").map_err(|error| error.to_string())?;
        fs::write(lake.join("Lake.olean"), "").map_err(|error| error.to_string())?;
        Ok((dir, root, lake))
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
        let (_dir, root, lake) = fake_project("env")?;
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
        let (_dir, root, lake) = fake_project("env-invalid")?;
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
    fn a_caller_runs_the_command_and_hands_back_its_output() -> Result<(), String> {
        let (_dir, root, _) = fake_project("options")?;
        let sysroot = root.join("sysroot");
        let workspace = LakeWorkspace::new(&root)
            .lean_sysroot(&sysroot)
            .lean_num_threads(2)
            .config_option("flag", "true")
            .reconfigure();
        let command = workspace
            .built_shared_library_command("Demo")
            .map_err(|error| error.to_string())?;
        let call = LakeCallRecord::of(&command);
        assert_eq!(call.program, sysroot.join("bin/lake"));
        assert_eq!(call.args, ["-R", "-Kflag=true", "--no-build", "query", "Demo:shared"]);
        assert_eq!(call.dir.as_deref(), Some(root.as_path()));
        for env in [
            ("LEAN_SYSROOT".to_owned(), Some(sysroot.display().to_string())),
            ("LEAN_NUM_THREADS".to_owned(), Some("2".to_owned())),
            ("LEAN_PATH".to_owned(), None),
        ] {
            assert!(call.envs.contains(&env), "missing {env:?} in {:?}", call.envs);
        }
        let found = workspace
            .read_built_shared_library("Demo", &answer(3, "", ""))
            .map_err(|error| error.to_string())?;
        assert_eq!(found, None);
        let Err(LinkDiagnostics::LakeFailed { command, .. }) =
            workspace.read_built_shared_library("Demo", &answer(1, "", "error: Demo.lean:1:0: oops"))
        else {
            return Err("a failed query was not reported as failed".to_owned());
        };
        assert_eq!(command, "lake -R -Kflag=true --no-build query Demo:shared");
        Ok(())
    }

    #[test]
    fn a_query_returns_the_one_file_lake_prints() -> Result<(), String> {
        let (_dir, root, _) = fake_project("query")?;
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
        let (_dir, root, _) = fake_project("missing")?;
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
        let dir = temp_dir("no-lakefile")?;
        let mut fake = FakeLake::new(|_: &[String]| answer(0, "", ""));
        let result = LakeWorkspace::new(dir.path()).lean_path_with(&mut fake);
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
