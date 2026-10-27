use crate::{
    RunError, Runner,
    sandbox::{self, Finished, Spawn},
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) struct Probe {
    program: PathBuf,
    read: Vec<PathBuf>,
}

impl Probe {
    pub fn find(runner: &Runner) -> Result<Self, RunError> {
        let configured = &runner.cfg.python;
        let program = if configured.is_absolute() || configured.components().count() > 1 {
            std::path::absolute(configured)?
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(configured))
                .find(|p| p.is_file())
                .ok_or_else(|| {
                    RunError::unavailable(
                        format!("{} isn't installed.", configured.display()),
                        "Run scripts/setup-runner.sh and configure CALC_RUNNER_PYTHON.",
                    )
                })?
        };
        let real = program.canonicalize()?;
        let mut read = vec![program.clone(), real.clone()];
        let output = Command::new(&program)
            .args([
                "-I",
                "-B",
                "-c",
                "import json,sys; print(json.dumps([sys.prefix,sys.base_prefix]+sys.path))",
            ])
            .output()?;
        if !output.status.success() {
            return Err(RunError::unavailable(
                "The configured Python could not start.",
                "Run scripts/setup-runner.sh.",
            )
            .with_stderr(String::from_utf8_lossy(&output.stderr)));
        }
        let paths: Vec<String> =
            serde_json::from_slice(&output.stdout).map_err(RunError::internal)?;
        read.extend(
            paths
                .into_iter()
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        );
        Ok(Self { program, read })
    }
}

pub(crate) fn execute(
    runner: &Runner,
    scratch: &Path,
    args: Vec<OsString>,
    serve: &mut dyn FnMut(&[u8]) -> Vec<u8>,
) -> Result<Finished, RunError> {
    let probe = runner.probe()?;
    let threads = runner.cfg.limits.threads.max(1).to_string();
    let env = vec![
        ("HOME".into(), scratch.to_string_lossy().into_owned()),
        (
            "TMPDIR".into(),
            scratch.join("tmp").to_string_lossy().into_owned(),
        ),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PYTHONNOUSERSITE".into(), "1".into()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("OPENBLAS_NUM_THREADS".into(), threads.clone()),
        ("OMP_NUM_THREADS".into(), threads.clone()),
        ("ARROW_NUM_THREADS".into(), threads),
        ("LANG".into(), "en_US.UTF-8".into()),
        ("TZ".into(), "UTC".into()),
        ("OPENSSL_CONF".into(), "/dev/null".into()),
    ];
    sandbox::run(
        &runner.sandbox()?,
        &Spawn {
            program: probe.program.clone(),
            args,
            env,
            scratch,
            read: probe.read.clone(),
            exec: vec![probe.program.clone()],
            limits: &runner.cfg.limits,
            address_space: true,
            desktop_converter: false,
        },
        serve,
    )
}
