//! The cable path to the panel.
//!
//! Over Wi-Fi the panel dials this machine's address directly, so nothing on
//! this side has to be arranged. Over USB there is no IP route at all: the only
//! way in is the debug bridge's reverse tunnel, which maps a port on the panel
//! onto a port here. That tunnel is state held by the `adb` server, not by this
//! program, so it has to be opened every time the link is switched and every
//! time the server restarts underneath us.
//!
//! Everything here is a subprocess. `adb` is not a library, the protocol it
//! speaks is not this program's to implement, and a panel that fails to answer
//! must not take the publisher's main thread with it — the caller runs these on
//! a worker.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bongocat_ui::DeviceLinkProblem;

/// The Windows flag that keeps a console window from appearing.
///
/// This build has no console of its own, and every one of these calls would
/// otherwise flash a black rectangle on a desktop that is supposed to show a cat
/// and nothing else.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The activity a relink has to be delivered to.
const ACTIVITY: &str = "com.bongocat.catscreen/.MainActivity";

/// `FLAG_ACTIVITY_SINGLE_TOP`.
///
/// Without it, `am start` puts a second copy of the activity on top of the one
/// already drawing, which costs a full surface rebuild and leaves the old one
/// alive behind it. With it, the running instance receives the new address as a
/// new intent — that is what makes moving the panel a reconnect rather than a
/// restart.
const SINGLE_TOP: &str = "0x20000000";

/// Finds an `adb` to talk to.
///
/// The configured path wins, then the environment, then the PATH itself, then
/// the usual SDK location. The last one is only a convenience: an SDK installed
/// somewhere unusual is reached by the configured path, which the settings file
/// keeps.
pub(crate) fn locate(configured: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = configured.filter(|path| !path.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("MIAOSCREEN_ADB") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("adb"));
    // The usual SDK locations and the SDK this fork is built against. A machine
    // that keeps its tools somewhere else sets the path in the settings file or
    // in `MIAOSCREEN_ADB`, both of which are checked first.
    for root in sdk_roots() {
        candidates.push(root.join("platform-tools").join(ADB_EXECUTABLE));
    }
    candidates
        .into_iter()
        .find(|candidate| command(candidate).arg("version").output().is_ok())
}

/// The executable name, which differs on the one platform this ships for.
#[cfg(target_os = "windows")]
const ADB_EXECUTABLE: &str = "adb.exe";
#[cfg(not(target_os = "windows"))]
const ADB_EXECUTABLE: &str = "adb";

fn sdk_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for variable in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = std::env::var_os(variable) {
            roots.push(PathBuf::from(root));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("Android").join("Sdk"));
    }
    // The SDK `scripts/build-apk.ps1` compiles the panel against, so the same
    // installation that can build the app can also reach it over the cable.
    roots.push(PathBuf::from(
        "D:/yuequedeng/bambu-monitor-android/.tools/android-sdk",
    ));
    roots
}

fn command(adb: &Path) -> Command {
    let mut command = Command::new(adb);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Runs one `adb` invocation and returns its standard output.
fn run(adb: &Path, args: &[&str]) -> Result<String, String> {
    let output = command(adb)
        .args(args)
        .output()
        .map_err(|error| format!("{}: {error}", adb.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_owned());
    }
    Ok(stdout)
}

/// The serial of the first device that is ready to talk.
///
/// A device that is attached but still authorising shows up as `unauthorized`,
/// and one that is mid-restart as `offline`; neither can carry a tunnel, so both
/// are treated as "no cable yet" rather than as a transport error.
fn serial(adb: &Path) -> Result<String, DeviceLinkProblem> {
    let listing = run(adb, &["devices"]).map_err(|_| DeviceLinkProblem::NoCable)?;
    listing
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?;
            let state = fields.next()?;
            (state == "device").then(|| serial.to_owned())
        })
        .next()
        .ok_or(DeviceLinkProblem::NoCable)
}

/// Opens the reverse tunnel that makes this machine's port reachable on the panel.
pub(crate) fn prepare(adb: &Path, port: u16) -> Result<(), DeviceLinkProblem> {
    let serial = serial(adb)?;
    let spec = format!("tcp:{port}");
    run(adb, &["-s", &serial, "reverse", &spec, &spec])
        .map_err(|_| DeviceLinkProblem::TunnelFailed)?;
    Ok(())
}

/// Starts the panel's app with `address`, or moves the running one to it.
///
/// This is the fallback for a panel that is not listening on any socket: the
/// relink message needs a connection to travel on, and a panel that is pointed
/// at an address nothing answers on has no way to learn a better one.
pub(crate) fn launch(adb: &Path, address: &str) -> Result<(), DeviceLinkProblem> {
    let serial = serial(adb)?;
    let output = run(
        adb,
        &[
            "-s", &serial, "shell", "am", "start", "-f", SINGLE_TOP, "-n", ACTIVITY, "--es",
            "address", address,
        ],
    )
    .map_err(|_| DeviceLinkProblem::LaunchFailed)?;
    // `am` reports a missing activity or a bad flag on standard output and still
    // exits zero, so the exit status alone would call that a success.
    if output.contains("Error:") || output.contains("Exception") {
        return Err(DeviceLinkProblem::LaunchFailed);
    }
    Ok(())
}
